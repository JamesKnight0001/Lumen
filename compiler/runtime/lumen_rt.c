/*
 * Lumen C runtime: the support library every native-compiled Lumen program
 * links against. It defines the NaN-boxed value representation, a conservative
 * generational mark/sweep garbage collector, the FFI / cbuf primitives, a
 * custom naked >-< asm setjmp/longjmp for try/catch, and a Win64 COM vtable
 * trampoline. Value semantics here must match the tree-walking interpreter.
 * 
 * THESE FUNCTIONS INTENTIALLY HAVE "LUMEN_" PREFIXES TO AVOID COLLISIONS WITH C LIBRARY NAMES,
 * DO NOT RENAME - JAMES <- MODIFY THIS FILE IF U KNOW WHAT YOU ARE DOING, DO NOT RENAME FUNCTIONS,
 * TO IDENTICAL TO EXISTING C LIBRARY FUNCTIONS!
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <sys/stat.h>
#include <dirent.h>
#include <unistd.h>
#include <time.h>

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#define popen _popen
#define pclose _pclose
#else
#include <sys/wait.h>
#endif

typedef uint64_t LumenVal;

/*
 * NaN-box layout. Every value is a 64-bit double-or-tagged word.
 *   - A real double is any bit pattern that is NOT a quiet NaN (QNAN bits clear).
 *   - A boxed value sets all QNAN bits; the top SIGN bit then distinguishes
 *     a heap pointer (SIGN=1, 48-bit payload = pointer) from an immediate
 *     (SIGN=0, 3-bit tag at TAG_SHIFT selects int/bool/nil, 48-bit payload).
 * Pointers and ints are limited to 48 bits, which is why integers wrap at 48.
 */
#define QNAN      0x7FF8000000000000ULL
#define SIGN      0x8000000000000000ULL
#define TAG_SHIFT 48
#define PAYLOAD_MASK ((1ULL << 48) - 1)

#define TAG_INT  1
#define TAG_BOOL 2
#define TAG_NIL  3

#define OBJ_STR    1
#define OBJ_LIST   2
#define OBJ_STRUCT 3
#define OBJ_MAP    4
#define OBJ_FUNC   5
#define OBJ_CBUF   6

typedef struct {
    int kind;
    int rc;
    int gc_mark;
} LumenObj;

typedef struct {
    int kind;
    int rc;
    int gc_mark;
    size_t len;
    char *data;
} LumenStr;

typedef struct {
    int kind;
    int rc;
    int gc_mark;
    size_t len;
    size_t cap;
    LumenVal *items;
} LumenList;

typedef struct {
    int kind;
    int rc;
    int gc_mark;
    size_t len;
    uint8_t *data;
} LumenCBuf;

typedef struct {
    int kind;
    int rc;
    int gc_mark;
    const char *name;
    size_t nfields;
    const char **field_names;
    LumenVal *field_vals;
} LumenStruct;

typedef struct {
    int kind;
    int rc;
    int gc_mark;
    size_t len;
    size_t cap;
    LumenVal *keys;
    LumenVal *vals;

    size_t *idx;
    size_t idx_cap;
} LumenMap;

typedef struct {
    int kind;
    int rc;
    int gc_mark;
    void *code;
    int arity;
    int ncap;
    LumenVal *caps;
} LumenFunc;

static inline int lumen_is_double(LumenVal v) { return (v & QNAN) != QNAN; }
static inline int lumen_is_boxed(LumenVal v)  { return (v & QNAN) == QNAN; }

LumenVal lumen_from_double(double d) {
    LumenVal v;
    memcpy(&v, &d, 8);
    return v;
}
double lumen_to_double(LumenVal v) {
    double d;
    memcpy(&d, &v, 8);
    return d;
}
LumenVal lumen_from_int(int64_t n) {

    return QNAN | ((uint64_t)TAG_INT << TAG_SHIFT) | ((uint64_t)n & PAYLOAD_MASK);
}
int64_t lumen_to_int(LumenVal v) {
    uint64_t p = v & PAYLOAD_MASK;

    // Sign-extend the 48-bit payload back to a full int64: if bit 47 is set the
    // value was negative, so fill the high 16 bits. This is the inverse of the
    // 48-bit wrap and must match interp's wrap48 round-trip exactly.
    if (p & (1ULL << 47)) p |= ~PAYLOAD_MASK;
    return (int64_t)p;
}
LumenVal lumen_true(void)  { return QNAN | ((uint64_t)TAG_BOOL << TAG_SHIFT) | 1; }
LumenVal lumen_false(void) { return QNAN | ((uint64_t)TAG_BOOL << TAG_SHIFT) | 0; }
LumenVal lumen_bool(int b) { return b ? lumen_true() : lumen_false(); }
LumenVal lumen_nil(void)   { return QNAN | ((uint64_t)TAG_NIL << TAG_SHIFT); }

static LumenVal box_ptr(void *p) {
    return QNAN | SIGN | ((uint64_t)(uintptr_t)p & PAYLOAD_MASK);
}
static void *unbox_ptr(LumenVal v) {
    return (void *)(uintptr_t)(v & PAYLOAD_MASK);
}

static int tag_of(LumenVal v) { return (int)((v >> TAG_SHIFT) & 0x7); }

int lumen_is_int(LumenVal v)   { return lumen_is_boxed(v) && !(v & SIGN) && tag_of(v) == TAG_INT; }
int lumen_is_bool(LumenVal v)  { return lumen_is_boxed(v) && !(v & SIGN) && tag_of(v) == TAG_BOOL; }
int lumen_is_nil(LumenVal v)   { return lumen_is_boxed(v) && !(v & SIGN) && tag_of(v) == TAG_NIL; }
int lumen_is_ptr(LumenVal v)   { return lumen_is_boxed(v) && (v & SIGN); }

uint32_t lumen_current_line = 0;
void lumen_set_line(uint32_t n) { lumen_current_line = n; }

#include <stdint.h>
#define LUMEN_TRY_MAX 256
typedef struct { void *slot[8]; } LumenJmp;
static LumenJmp lumen_try_stack[LUMEN_TRY_MAX];
static int lumen_try_depth = 0;
static char lumen_err_buf[512];

LumenVal lumen_str_new(const char *cstr);
const char *lumen_cstr(LumenVal v);
int64_t lumen_ffi_argint(LumenVal v);
int64_t lumen_ffi_argdouble(LumenVal v);

#if defined(__GNUC__) || defined(__clang__)
// Custom setjmp/longjmp for try/catch. We roll our own (instead of libc setjmp)
// to control the exact save layout: callee-saved regs rbx/rbp/r12-r15, the
// caller's rsp, and the return address, packed into LumenJmp.slot[0..8]. Naked
// + asm so the compiler adds no prologue that would corrupt the saved frame.
// Win64 ABI: first arg (buf) arrives in rcx.
__attribute__((naked, noinline)) int lumen_setjmp(LumenJmp *buf __attribute__((unused))) {
    __asm__ volatile(
        "movq %rbx, 0(%rcx)\n\t"
        "movq %rbp, 8(%rcx)\n\t"
        "movq %r12, 16(%rcx)\n\t"
        "movq %r13, 24(%rcx)\n\t"
        "movq %r14, 32(%rcx)\n\t"
        "movq %r15, 40(%rcx)\n\t"
        "leaq 8(%rsp), %rax\n\t"
        "movq %rax, 48(%rcx)\n\t"
        "movq (%rsp), %rax\n\t"
        "movq %rax, 56(%rcx)\n\t"
        "xorl %eax, %eax\n\t"
        "ret\n\t");
}

// Restore the frame saved by lumen_setjmp and resume there, returning `val`
// (forced to 1 if zero, so the catch path always sees a nonzero result).
// Win64 args: buf in rcx, val in edx.
__attribute__((naked, noinline, noreturn)) void lumen_longjmp(LumenJmp *buf __attribute__((unused)), int val __attribute__((unused))) {
    __asm__ volatile(
        "movq 0(%rcx), %rbx\n\t"
        "movq 8(%rcx), %rbp\n\t"
        "movq 16(%rcx), %r12\n\t"
        "movq 24(%rcx), %r13\n\t"
        "movq 32(%rcx), %r14\n\t"
        "movq 40(%rcx), %r15\n\t"
        "movq 48(%rcx), %rsp\n\t"
        "movl %edx, %eax\n\t"
        "testl %eax, %eax\n\t"
        "jnz 1f\n\t"
        "movl $1, %eax\n\t"
        "1:\n\t"
        "jmp *56(%rcx)\n\t");
}
#else
#error "Lumen runtime requires GCC/Clang for try/catch (naked asm setjmp)"
#endif

LumenJmp *lumen_try_push(void) {
    if (lumen_try_depth >= LUMEN_TRY_MAX) return NULL;
    return &lumen_try_stack[lumen_try_depth++];
}

void lumen_try_pop(void) {
    if (lumen_try_depth > 0) lumen_try_depth--;
}

LumenVal lumen_caught_msg(void) { return lumen_str_new(lumen_err_buf); }

#if defined(__GNUC__) || defined(__clang__)
__attribute__((noreturn))
#endif
static void lumen_die(const char *msg) {
    // If we are inside a try, stash the message and longjmp back to the nearest
    // catch instead of aborting. With no active try this is a fatal error: print
    // the message (with source line if known) and exit.
    if (lumen_try_depth > 0) {

        snprintf(lumen_err_buf, sizeof(lumen_err_buf), "%s", msg);
        lumen_try_depth--;
        lumen_longjmp(&lumen_try_stack[lumen_try_depth], 1);
    }
    if (lumen_current_line)
        fprintf(stderr, "error: %s\n  --> line %u\n", msg, lumen_current_line);
    else
        fprintf(stderr, "error: %s\n", msg);
    exit(1);
}

LumenVal lumen_raise(LumenVal msg) {
    lumen_die(lumen_cstr(msg));
    return lumen_nil();
}

static long lumen_live_allocs = 0;
static long lumen_total_allocs = 0;

long lumen_allocs_since_gc = 0;

static void *xalloc(size_t n) {
    void *p = calloc(1, n);
    if (!p) { fprintf(stderr, "lumen: out of memory\n"); exit(1); }
    lumen_live_allocs++;
    lumen_total_allocs++;
    lumen_allocs_since_gc++;
    return p;
}

static void xfree(void *p) {
    if (p) {
        free(p);
        lumen_live_allocs--;
    }
}

long lumen_live_count(void) { return lumen_live_allocs; }
long lumen_total_count(void) { return lumen_total_allocs; }

static void **gc_objects = NULL;
static size_t gc_count = 0;
// gc_old marks the boundary between the "old" generation (indices < gc_old,
// already survived a major collection) and the young nursery (>= gc_old).
// Minor collections only sweep the nursery; every 8th run promotes via a major.
static size_t gc_old = 0;
static size_t gc_minor_runs = 0;
static size_t gc_cap = 0;
static void *gc_stack_bottom = NULL;
static int gc_enabled = 0;

static int gc_pause_depth = 0;

// gc_set is an open-addressed pointer hash set mirroring gc_objects. It lets the
// conservative scanner answer "is this word a pointer we manage?" in O(1) so a
// random stack word that merely looks like a pointer is not treated as live.
static void **gc_set = NULL;
static size_t gc_set_cap = 0;
static size_t gc_set_len = 0;

static inline size_t gc_hash_ptr(void *p) {

    uint64_t x = (uint64_t)(uintptr_t)p;
    x *= 0x9E3779B97F4A7C15ull;
    return (size_t)(x >> 32);
}

static void gc_set_insert_raw(void **tbl, size_t cap, void *p) {
    size_t mask = cap - 1;
    size_t i = gc_hash_ptr(p) & mask;
    while (tbl[i]) {
        if (tbl[i] == p) return;
        i = (i + 1) & mask;
    }
    tbl[i] = p;
}

static void gc_set_grow(size_t want) {
    size_t cap = gc_set_cap ? gc_set_cap : 512;
    while (cap < want * 2) cap *= 2;
    if (cap == gc_set_cap) return;
    void **tbl = calloc(cap, sizeof(void *));
    if (!tbl) { fprintf(stderr, "lumen: gc set oom\n"); exit(1); }
    for (size_t i = 0; i < gc_set_cap; i++)
        if (gc_set[i]) gc_set_insert_raw(tbl, cap, gc_set[i]);
    free(gc_set);
    gc_set = tbl;
    gc_set_cap = cap;
}

static void gc_set_add(void *p) {
    if ((gc_set_len + 1) * 2 >= gc_set_cap) gc_set_grow(gc_set_len + 1);
    size_t before = gc_set_len;

    size_t mask = gc_set_cap - 1;
    size_t i = gc_hash_ptr(p) & mask;
    while (gc_set[i]) { if (gc_set[i] == p) return; i = (i + 1) & mask; }
    gc_set[i] = p;
    gc_set_len = before + 1;
}

static void gc_set_rebuild(void) {
    if (gc_set_cap) memset(gc_set, 0, gc_set_cap * sizeof(void *));
    gc_set_len = 0;
    if (gc_count * 2 >= gc_set_cap) gc_set_grow(gc_count);
    for (size_t i = 0; i < gc_count; i++) gc_set_add(gc_objects[i]);
}

static void gc_register(void *o) {
    if (gc_count == gc_cap) {
        gc_cap = gc_cap ? gc_cap * 2 : 256;
        gc_objects = realloc(gc_objects, gc_cap * sizeof(void *));
        if (!gc_objects) { fprintf(stderr, "lumen: gc oom\n"); exit(1); }
    }
    gc_objects[gc_count++] = o;
    gc_set_add(o);
}

static int gc_is_object(void *p) {
    if (!gc_set_cap) return 0;
    size_t mask = gc_set_cap - 1;
    size_t i = gc_hash_ptr(p) & mask;
    while (gc_set[i]) {
        if (gc_set[i] == p) return 1;
        i = (i + 1) & mask;
    }
    return 0;
}

static void gc_mark_val(LumenVal v);

static void gc_mark_obj(void *p) {
    LumenObj *o = (LumenObj *)p;
    if (o->gc_mark) return;
    o->gc_mark = 1;
    switch (o->kind) {
        case OBJ_LIST: {
            LumenList *l = (LumenList *)o;
            for (size_t i = 0; i < l->len; i++) gc_mark_val(l->items[i]);
            break;
        }
        case OBJ_MAP: {
            LumenMap *m = (LumenMap *)o;
            for (size_t i = 0; i < m->len; i++) { gc_mark_val(m->keys[i]); gc_mark_val(m->vals[i]); }
            break;
        }
        case OBJ_STRUCT: {
            LumenStruct *s = (LumenStruct *)o;
            for (size_t i = 0; i < s->nfields; i++) gc_mark_val(s->field_vals[i]);
            break;
        }
        case OBJ_FUNC: {

            LumenFunc *f = (LumenFunc *)o;
            for (int i = 0; i < f->ncap; i++) gc_mark_val(f->caps[i]);
            break;
        }
        default: break;
    }
}

static void gc_mark_val(LumenVal v) {
    if (lumen_is_ptr(v)) {
        void *p = unbox_ptr(v);
        if (gc_is_object(p)) gc_mark_obj(p);
    }
}

static void gc_scan_range(void *lo, void *hi) {
    uintptr_t a = (uintptr_t)lo, b = (uintptr_t)hi;
    // Conservative scan: walk the stack range one aligned word at a time and mark
    // any word that both looks NaN-boxed and is a registered object. We never move
    // objects, so treating an ambiguous word as a root is always safe (at worst it
    // keeps something alive a little longer).
    a = (a + 7) & ~(uintptr_t)7;
    b = b & ~(uintptr_t)7;
    uint64_t *p = (uint64_t *)a;
    uint64_t *end = (uint64_t *)b;
    for (; p < end; p++) {
        LumenVal w = *p;
        if (lumen_is_ptr(w)) {
            void *obj = unbox_ptr(w);
            if (gc_is_object(obj)) gc_mark_obj(obj);
        }
    }
}

static void gc_free_obj(LumenObj *o) {
    switch (o->kind) {
        case OBJ_STR:    xfree(((LumenStr *)o)->data); break;
        case OBJ_LIST:   xfree(((LumenList *)o)->items); break;
        case OBJ_MAP:    xfree(((LumenMap *)o)->keys); xfree(((LumenMap *)o)->vals); xfree(((LumenMap *)o)->idx); break;
        case OBJ_STRUCT: xfree(((LumenStruct *)o)->field_vals); break;
        case OBJ_FUNC:   xfree(((LumenFunc *)o)->caps); break;
        case OBJ_CBUF:   xfree(((LumenCBuf *)o)->data); break;
        default: break;
    }
    xfree(o);
}

void lumen_gc_collect(void) {
    if (!gc_enabled || !gc_stack_bottom) return;
    if (gc_pause_depth > 0) return;

    // Amortize: only collect once enough has been allocated since the last run.
    if (lumen_allocs_since_gc < 512) return;
    lumen_allocs_since_gc = 0;
    volatile void *probe = NULL;
    void *stack_top = (void *)&probe;
    void *lo = stack_top, *hi = gc_stack_bottom;
    if (lo > hi) { void *t = lo; lo = hi; hi = t; }

    // Every 8th collection is a major (whole heap); the rest are minor (nursery
    // only). Roots are found by scanning the live C stack conservatively.
    int major = (++gc_minor_runs >= 8);

    for (size_t i = 0; i < gc_count; i++) ((LumenObj *)gc_objects[i])->gc_mark = 0;

    gc_scan_range(lo, hi);

    if (major) {
        // Sweep the entire object array, compacting survivors down. Everything
        // that survives becomes "old" (gc_old = w), and the minor counter resets.
        size_t w = 0;
        for (size_t i = 0; i < gc_count; i++) {
            LumenObj *o = (LumenObj *)gc_objects[i];
            if (o->gc_mark) gc_objects[w++] = o;
            else gc_free_obj(o);
        }
        gc_count = w;
        gc_old = w;
        gc_minor_runs = 0;
    } else {
        // Minor: only sweep from gc_old onward, leaving the old generation
        // untouched. Old objects keep whatever mark they have without being freed.
        size_t w = gc_old;
        for (size_t i = gc_old; i < gc_count; i++) {
            LumenObj *o = (LumenObj *)gc_objects[i];
            if (o->gc_mark) gc_objects[w++] = o;
            else gc_free_obj(o);
        }
        gc_count = w;
        gc_old = w;
    }

    gc_set_rebuild();
}

void lumen_gc_report(void) {
    fprintf(stderr,
            "[lumen gc] total allocations: %ld, still live at exit: %ld\n",
            lumen_total_allocs, lumen_live_allocs);
}
void lumen_gc_init(void *stack_bottom) {
    gc_stack_bottom = stack_bottom;
    gc_enabled = 1;
    const char *e = getenv("LUMEN_GC");
    if (e && e[0] == '1') atexit(lumen_gc_report);
}

void lumen_release(LumenVal v);

void lumen_retain(LumenVal v) {
    if (lumen_is_ptr(v)) {
        ((LumenObj *)unbox_ptr(v))->rc++;
    }
}

void lumen_release(LumenVal v) {
    if (!lumen_is_ptr(v)) return;
    LumenObj *o = (LumenObj *)unbox_ptr(v);
    if (--o->rc > 0) return;
    switch (o->kind) {
        case OBJ_STR:
            xfree(((LumenStr *)o)->data);
            break;
        case OBJ_LIST: {
            LumenList *l = (LumenList *)o;
            for (size_t i = 0; i < l->len; i++) lumen_release(l->items[i]);
            xfree(l->items);
            break;
        }
        case OBJ_MAP: {
            LumenMap *m = (LumenMap *)o;
            for (size_t i = 0; i < m->len; i++) {
                lumen_release(m->keys[i]);
                lumen_release(m->vals[i]);
            }
            xfree(m->keys);
            xfree(m->vals);
            xfree(m->idx);
            break;
        }
        case OBJ_STRUCT: {
            LumenStruct *s = (LumenStruct *)o;
            for (size_t i = 0; i < s->nfields; i++) lumen_release(s->field_vals[i]);
            xfree(s->field_vals);
            break;
        }
        case OBJ_FUNC:
        default:
            break;
    }
    xfree(o);
}

LumenVal lumen_str_new(const char *cstr) {
    LumenStr *s = xalloc(sizeof(LumenStr));
    gc_register(s);
    s->kind = OBJ_STR;
    s->rc = 1;
    s->len = strlen(cstr);
    s->data = xalloc(s->len + 1);
    memcpy(s->data, cstr, s->len + 1);
    return box_ptr(s);
}

LumenVal lumen_str_concat(LumenVal a, LumenVal b) {
    LumenStr *sa = unbox_ptr(a);
    LumenStr *sb = unbox_ptr(b);
    LumenStr *r = xalloc(sizeof(LumenStr));
    gc_register(r);
    r->kind = OBJ_STR;
    r->rc = 1;
    r->len = sa->len + sb->len;
    r->data = xalloc(r->len + 1);
    memcpy(r->data, sa->data, sa->len);
    memcpy(r->data + sa->len, sb->data, sb->len + 1);
    return box_ptr(r);
}

LumenVal lumen_list_new(int64_t n) {
    LumenList *l = xalloc(sizeof(LumenList));
    gc_register(l);
    l->kind = OBJ_LIST;
    l->rc = 1;
    l->len = 0;
    l->cap = n > 0 ? (size_t)n : 4;
    l->items = xalloc(l->cap * sizeof(LumenVal));
    return box_ptr(l);
}
void lumen_list_push(LumenVal lst, LumenVal v) {
    LumenList *l = unbox_ptr(lst);
    if (l->len == l->cap) {
        l->cap *= 2;
        l->items = realloc(l->items, l->cap * sizeof(LumenVal));
        if (!l->items) { fprintf(stderr, "lumen: oom\n"); exit(1); }
    }
    l->items[l->len++] = v;
}
LumenVal lumen_list_get(LumenVal lst, int64_t i) {
    LumenList *l = unbox_ptr(lst);
    if (i < 0 || (size_t)i >= l->len) { lumen_die("index out of range"); }
    return l->items[i];
}
void lumen_list_set(LumenVal lst, int64_t i, LumenVal v) {
    LumenList *l = unbox_ptr(lst);
    if (i < 0 || (size_t)i >= l->len) { lumen_die("index out of range"); }
    l->items[i] = v;
}
int64_t lumen_len(LumenVal v) {
    if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        if (o->kind == OBJ_LIST) return (int64_t)((LumenList *)o)->len;
        if (o->kind == OBJ_STR)  return (int64_t)((LumenStr *)o)->len;
        if (o->kind == OBJ_MAP)  return (int64_t)((LumenMap *)o)->len;
    }
    lumen_die("len() of non-collection");
}

LumenVal lumen_struct_new(const char *name, int64_t nfields, const char **field_names) {
    LumenStruct *s = xalloc(sizeof(LumenStruct));
    gc_register(s);
    s->kind = OBJ_STRUCT;
    s->rc = 1;
    s->name = name;
    s->nfields = (size_t)nfields;
    s->field_names = field_names;
    s->field_vals = xalloc((nfields ? nfields : 1) * sizeof(LumenVal));
    for (int64_t i = 0; i < nfields; i++) s->field_vals[i] = lumen_nil();
    return box_ptr(s);
}
static int field_index(LumenStruct *s, const char *fname) {
    for (size_t i = 0; i < s->nfields; i++)
        if (strcmp(s->field_names[i], fname) == 0) return (int)i;
    return -1;
}
void lumen_struct_set(LumenVal sv, const char *fname, LumenVal v) {
    LumenStruct *s = unbox_ptr(sv);
    int i = field_index(s, fname);
    if (i < 0) { char b[128]; snprintf(b, sizeof b, "no field '%s'", fname); lumen_die(b); }
    s->field_vals[i] = v;
}
LumenVal lumen_struct_get(LumenVal sv, const char *fname) {
    LumenStruct *s = unbox_ptr(sv);
    int i = field_index(s, fname);
    if (i < 0) { char b[128]; snprintf(b, sizeof b, "no field '%s'", fname); lumen_die(b); }
    return s->field_vals[i];
}

static double as_num(LumenVal v) {
    if (lumen_is_double(v)) return lumen_to_double(v);
    if (lumen_is_int(v))    return (double)lumen_to_int(v);
    lumen_die("arithmetic on non-number");
}
static int both_int(LumenVal a, LumenVal b) { return lumen_is_int(a) && lumen_is_int(b); }

LumenVal lumen_add(LumenVal a, LumenVal b) {
    if (lumen_is_ptr(a) && lumen_is_ptr(b)) {
        LumenObj *o = unbox_ptr(a);
        if (o->kind == OBJ_STR) return lumen_str_concat(a, b);
    }
    if (both_int(a, b)) return lumen_from_int(lumen_to_int(a) + lumen_to_int(b));
    return lumen_from_double(as_num(a) + as_num(b));
}
LumenVal lumen_sub(LumenVal a, LumenVal b) {
    if (both_int(a, b)) return lumen_from_int(lumen_to_int(a) - lumen_to_int(b));
    return lumen_from_double(as_num(a) - as_num(b));
}
LumenVal lumen_mul(LumenVal a, LumenVal b) {
    if (both_int(a, b)) return lumen_from_int(lumen_to_int(a) * lumen_to_int(b));
    return lumen_from_double(as_num(a) * as_num(b));
}

LumenVal lumen_pow(LumenVal a, LumenVal b) {
    if (both_int(a, b)) {
        int64_t base = lumen_to_int(a);
        int64_t exp = lumen_to_int(b);
        if (exp >= 0) {
            int64_t acc = 1;
            for (int64_t i = 0; i < exp; i++) {

                acc = lumen_to_int(lumen_from_int(acc * base));
            }
            return lumen_from_int(acc);
        }
        return lumen_from_double(pow((double)base, (double)exp));
    }
    return lumen_from_double(pow(as_num(a), as_num(b)));
}
LumenVal lumen_div(LumenVal a, LumenVal b) {
    if (both_int(a, b)) {
        int64_t d = lumen_to_int(b);
        if (d == 0) { lumen_die("division by zero"); }
        return lumen_from_int(lumen_to_int(a) / d);
    }
    return lumen_from_double(as_num(a) / as_num(b));
}
LumenVal lumen_mod(LumenVal a, LumenVal b) {
    if (both_int(a, b)) {
        int64_t d = lumen_to_int(b);
        if (d == 0) { lumen_die("modulo by zero"); }
        return lumen_from_int(lumen_to_int(a) % d);
    }
    double bb = as_num(b);
    return lumen_from_double(as_num(a) - bb * (double)(int64_t)(as_num(a) / bb));
}
LumenVal lumen_neg(LumenVal a) {
    if (lumen_is_int(a)) return lumen_from_int(-lumen_to_int(a));
    return lumen_from_double(-as_num(a));
}

static int num_cmp(LumenVal a, LumenVal b) {

    if (lumen_is_ptr(a) && lumen_is_ptr(b)
        && ((LumenObj *)unbox_ptr(a))->kind == OBJ_STR
        && ((LumenObj *)unbox_ptr(b))->kind == OBJ_STR) {
        int r = strcmp(((LumenStr *)unbox_ptr(a))->data, ((LumenStr *)unbox_ptr(b))->data);
        return r < 0 ? -1 : (r > 0 ? 1 : 0);
    }
    double x = as_num(a), y = as_num(b);
    return x < y ? -1 : (x > y ? 1 : 0);
}
LumenVal lumen_lt(LumenVal a, LumenVal b) { return lumen_bool(num_cmp(a, b) < 0); }
LumenVal lumen_le(LumenVal a, LumenVal b) { return lumen_bool(num_cmp(a, b) <= 0); }
LumenVal lumen_gt(LumenVal a, LumenVal b) { return lumen_bool(num_cmp(a, b) > 0); }
LumenVal lumen_ge(LumenVal a, LumenVal b) { return lumen_bool(num_cmp(a, b) >= 0); }

int lumen_truthy(LumenVal v) {
    if (lumen_is_bool(v)) return (int)(v & 1);
    if (lumen_is_nil(v))  return 0;
    lumen_die("condition must be a bool (strict truthiness)");
}

static int vals_eq(LumenVal a, LumenVal b) {
    if (lumen_is_int(a) && lumen_is_int(b)) return lumen_to_int(a) == lumen_to_int(b);
    if (lumen_is_double(a) || lumen_is_double(b)) {
        if ((lumen_is_double(a) || lumen_is_int(a)) && (lumen_is_double(b) || lumen_is_int(b)))
            return as_num(a) == as_num(b);
    }
    if (lumen_is_bool(a) && lumen_is_bool(b)) return (a & 1) == (b & 1);
    if (lumen_is_nil(a) && lumen_is_nil(b)) return 1;
    if (lumen_is_ptr(a) && lumen_is_ptr(b)) {
        LumenObj *oa = unbox_ptr(a), *ob = unbox_ptr(b);
        if (oa->kind == OBJ_STR && ob->kind == OBJ_STR)
            return strcmp(((LumenStr*)oa)->data, ((LumenStr*)ob)->data) == 0;
        return a == b;
    }
    return 0;
}
LumenVal lumen_eq(LumenVal a, LumenVal b) { return lumen_bool(vals_eq(a, b)); }
LumenVal lumen_ne(LumenVal a, LumenVal b) { return lumen_bool(!vals_eq(a, b)); }

static size_t val_hash(LumenVal v) {
    uint64_t h;
    // Hash canonicalization: an int and a float with the same numeric value must
    // hash (and compare) equal, so a whole-valued double is folded to its int64
    // form before hashing. Only non-integral or out-of-range floats hash by bits.
    if (lumen_is_int(v) || lumen_is_double(v)) {

        double d = as_num(v);
        if (isfinite(d) && floor(d) == d && d >= -9.2e18 && d <= 9.2e18) {
            int64_t iv = (int64_t)d;
            h = (uint64_t)iv;
        } else {
            uint64_t bits;
            memcpy(&bits, &d, 8);
            h = bits;
        }
    } else if (lumen_is_ptr(v) && ((LumenObj *)unbox_ptr(v))->kind == OBJ_STR) {

        LumenStr *s = (LumenStr *)unbox_ptr(v);
        h = 1469598103934665603ull;
        for (size_t i = 0; i < s->len; i++) {
            h ^= (unsigned char)s->data[i];
            h *= 1099511628211ull;
        }
    } else {

        h = (uint64_t)v;
    }

    h *= 0x9E3779B97F4A7C15ull;
    return (size_t)h;
}

static void fmt_double(double d, char *out, size_t cap) {

    if (isfinite(d) && floor(d) == d) {
        snprintf(out, cap, "%.1f", d);
        return;
    }

    char tmp[64];
    int prec = 17;
    for (int p = 1; p <= 17; p++) {
        snprintf(tmp, sizeof tmp, "%.*g", p, d);
        if (strtod(tmp, NULL) == d) { prec = p; break; }
    }

    if (strchr(tmp, 'e') || strchr(tmp, 'E')) {

        double ad = d < 0 ? -d : d;
        int exp10 = (ad > 0.0) ? (int)floor(log10(ad)) : 0;
        int decimals = prec - 1 - exp10;
        if (decimals < 0) decimals = 0;
        if (decimals > 330) decimals = 330;
        snprintf(out, cap, "%.*f", decimals, d);

        char *dot = strchr(out, '.');
        if (dot) {
            char *end = out + strlen(out) - 1;
            while (end > dot + 1 && *end == '0') *end-- = 0;
        }
    } else {
        snprintf(out, cap, "%s", tmp);
    }
}

static void print_val(LumenVal v, int top);

static void print_inner(LumenVal v) { print_val(v, 0); }

static void print_val(LumenVal v, int top) {
    if (lumen_is_double(v)) {
        char nb[64];
        fmt_double(lumen_to_double(v), nb, sizeof nb);
        printf("%s", nb);
    } else if (lumen_is_int(v)) {
        printf("%lld", (long long)lumen_to_int(v));
    } else if (lumen_is_bool(v)) {
        printf(v & 1 ? "true" : "false");
    } else if (lumen_is_nil(v)) {
        printf("nil");
    } else if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        if (o->kind == OBJ_STR) {
            LumenStr *s = (LumenStr*)o;
            if (top) printf("%s", s->data);
            else printf("\"%s\"", s->data);
        } else if (o->kind == OBJ_LIST) {
            LumenList *l = (LumenList*)o;
            printf("[");
            for (size_t i = 0; i < l->len; i++) { if (i) printf(", "); print_inner(l->items[i]); }
            printf("]");
        } else if (o->kind == OBJ_STRUCT) {
            LumenStruct *s = (LumenStruct*)o;
            printf("%s(", s->name);
            for (size_t i = 0; i < s->nfields; i++) {
                if (i) printf(", ");
                printf("%s: ", s->field_names[i]);
                print_inner(s->field_vals[i]);
            }
            printf(")");
        } else if (o->kind == OBJ_MAP) {
            LumenMap *m = (LumenMap*)o;
            printf("{");
            for (size_t i = 0; i < m->len; i++) {
                if (i) printf(", ");
                print_inner(m->keys[i]);
                printf(": ");
                print_inner(m->vals[i]);
            }
            printf("}");
        } else if (o->kind == OBJ_FUNC) {
            printf("<fn>");
        }
    }
}

void lumen_print(LumenVal v) { print_val(v, 1); printf("\n"); }

/* Multi-arg print(): each value at top-level, space-separated, one trailing
   newline  matching the interpreter's parts.join(" ") + newline. */
void lumen_print_part(LumenVal v) { print_val(v, 1); }
void lumen_print_space(void) { printf(" "); }
void lumen_print_nl(void) { printf("\n"); }

/* Growable string buffer + a buffer-writing mirror of print_val, so str() of a
   list/map/struct/fn produces the SAME text print() shows. Both backends must
   agree: this matches the interpreter's Display (top-level unquoted strings,
   nested strings quoted via repr). */
typedef struct { char *p; size_t len, cap; } SBuf;
static void sb_need(SBuf *b, size_t extra) {
    if (b->len + extra + 1 > b->cap) {
        while (b->len + extra + 1 > b->cap) b->cap = b->cap ? b->cap * 2 : 64;
        b->p = realloc(b->p, b->cap);
    }
}
static void sb_puts(SBuf *b, const char *s) {
    size_t n = strlen(s);
    sb_need(b, n);
    memcpy(b->p + b->len, s, n);
    b->len += n;
    b->p[b->len] = 0;
}
static void sfmt_val(SBuf *b, LumenVal v, int top) {
    char nb[64];
    if (lumen_is_double(v)) {
        fmt_double(lumen_to_double(v), nb, sizeof nb);
        sb_puts(b, nb);
    } else if (lumen_is_int(v)) {
        snprintf(nb, sizeof nb, "%lld", (long long)lumen_to_int(v));
        sb_puts(b, nb);
    } else if (lumen_is_bool(v)) {
        sb_puts(b, (v & 1) ? "true" : "false");
    } else if (lumen_is_nil(v)) {
        sb_puts(b, "nil");
    } else if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        if (o->kind == OBJ_STR) {
            LumenStr *s = (LumenStr *)o;
            if (top) { sb_puts(b, s->data); }
            else { sb_puts(b, "\""); sb_puts(b, s->data); sb_puts(b, "\""); }
        } else if (o->kind == OBJ_LIST) {
            LumenList *l = (LumenList *)o;
            sb_puts(b, "[");
            for (size_t i = 0; i < l->len; i++) { if (i) sb_puts(b, ", "); sfmt_val(b, l->items[i], 0); }
            sb_puts(b, "]");
        } else if (o->kind == OBJ_STRUCT) {
            LumenStruct *s = (LumenStruct *)o;
            sb_puts(b, s->name); sb_puts(b, "(");
            for (size_t i = 0; i < s->nfields; i++) {
                if (i) sb_puts(b, ", ");
                sb_puts(b, s->field_names[i]); sb_puts(b, ": ");
                sfmt_val(b, s->field_vals[i], 0);
            }
            sb_puts(b, ")");
        } else if (o->kind == OBJ_MAP) {
            LumenMap *m = (LumenMap *)o;
            sb_puts(b, "{");
            for (size_t i = 0; i < m->len; i++) {
                if (i) sb_puts(b, ", ");
                sfmt_val(b, m->keys[i], 0); sb_puts(b, ": "); sfmt_val(b, m->vals[i], 0);
            }
            sb_puts(b, "}");
        } else if (o->kind == OBJ_CBUF) {
            snprintf(nb, sizeof nb, "<cbuf %zu>", ((LumenCBuf *)o)->len);
            sb_puts(b, nb);
        } else if (o->kind == OBJ_FUNC) {
            sb_puts(b, "<fn>");
        }
    }
}
static LumenVal sfmt_to_str(LumenVal v) {
    SBuf b = {0};
    sb_need(&b, 0);
    b.p[0] = 0;
    sfmt_val(&b, v, 1);
    LumenVal r = lumen_str_new(b.p);
    free(b.p);
    return r;
}

LumenVal lumen_to_str(LumenVal v) {
    char buf[512];
    if (lumen_is_double(v)) {
        fmt_double(lumen_to_double(v), buf, sizeof buf);
    } else if (lumen_is_int(v)) {
        snprintf(buf, sizeof buf, "%lld", (long long)lumen_to_int(v));
    } else if (lumen_is_bool(v)) {
        snprintf(buf, sizeof buf, "%s", (v & 1) ? "true" : "false");
    } else if (lumen_is_nil(v)) {
        snprintf(buf, sizeof buf, "nil");
    } else if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        if (o->kind == OBJ_STR) return v;
        if (o->kind == OBJ_CBUF) {
            snprintf(buf, sizeof buf, "<cbuf %zu>", ((LumenCBuf *)o)->len);
        } else {
            // list/map/struct/fn: format via the same logic as print_val so
            // str(x) == what print(x) shows. Build into a temp file-less buffer
            // by redirecting through sfmt_val (mirrors print_val exactly).
            return sfmt_to_str(v);
        }
    } else {
        buf[0] = 0;
    }
    return lumen_str_new(buf);
}

LumenVal lumen_to_int_val(LumenVal v) {
    if (lumen_is_int(v)) return v;
    if (lumen_is_double(v)) return lumen_from_int((int64_t)lumen_to_double(v));
    if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        if (o->kind == OBJ_STR) return lumen_from_int(strtoll(((LumenStr*)o)->data, NULL, 10));
    }
    lumen_die("int() bad arg");
}
LumenVal lumen_to_float_val(LumenVal v) {
    if (lumen_is_double(v)) return v;
    if (lumen_is_int(v)) return lumen_from_double((double)lumen_to_int(v));
    if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        if (o->kind == OBJ_STR) return lumen_from_double(strtod(((LumenStr*)o)->data, NULL));
    }
    lumen_die("float() bad arg");
}

#include <math.h>
LumenVal lumen_math_sqrt(LumenVal v){ return lumen_from_double(sqrt(as_num(v))); }
LumenVal lumen_math_sin(LumenVal v){ return lumen_from_double(sin(as_num(v))); }
LumenVal lumen_math_cos(LumenVal v){ return lumen_from_double(cos(as_num(v))); }
LumenVal lumen_math_tan(LumenVal v){ return lumen_from_double(tan(as_num(v))); }
LumenVal lumen_math_abs(LumenVal v){ return lumen_from_double(fabs(as_num(v))); }
LumenVal lumen_math_floor(LumenVal v){ return lumen_from_double(floor(as_num(v))); }
LumenVal lumen_math_ceil(LumenVal v){ return lumen_from_double(ceil(as_num(v))); }
LumenVal lumen_math_pow(LumenVal a, LumenVal b){ return lumen_from_double(pow(as_num(a), as_num(b))); }
LumenVal lumen_math_log(LumenVal v){ return lumen_from_double(log(as_num(v))); }
LumenVal lumen_math_log10(LumenVal v){ return lumen_from_double(log10(as_num(v))); }
LumenVal lumen_math_exp(LumenVal v){ return lumen_from_double(exp(as_num(v))); }

LumenVal lumen_math_log2(LumenVal v){ return lumen_from_double(log2(as_num(v))); }
LumenVal lumen_math_cbrt(LumenVal v){ return lumen_from_double(cbrt(as_num(v))); }
LumenVal lumen_math_asin(LumenVal v){ return lumen_from_double(asin(as_num(v))); }
LumenVal lumen_math_acos(LumenVal v){ return lumen_from_double(acos(as_num(v))); }
LumenVal lumen_math_atan(LumenVal v){ return lumen_from_double(atan(as_num(v))); }
LumenVal lumen_math_atan2(LumenVal a, LumenVal b){ return lumen_from_double(atan2(as_num(a), as_num(b))); }
LumenVal lumen_math_sinh(LumenVal v){ return lumen_from_double(sinh(as_num(v))); }
LumenVal lumen_math_cosh(LumenVal v){ return lumen_from_double(cosh(as_num(v))); }
LumenVal lumen_math_tanh(LumenVal v){ return lumen_from_double(tanh(as_num(v))); }
LumenVal lumen_math_hypot(LumenVal a, LumenVal b){ return lumen_from_double(hypot(as_num(a), as_num(b))); }
LumenVal lumen_math_round(LumenVal v){ return lumen_from_double(round(as_num(v))); }
LumenVal lumen_math_trunc(LumenVal v){ return lumen_from_double(trunc(as_num(v))); }
LumenVal lumen_math_min(LumenVal a, LumenVal b){ double x=as_num(a), y=as_num(b); return lumen_from_double(x<y?x:y); }
LumenVal lumen_math_max(LumenVal a, LumenVal b){ double x=as_num(a), y=as_num(b); return lumen_from_double(x>y?x:y); }
LumenVal lumen_math_sign(LumenVal v){ double x=as_num(v); return lumen_from_double(x>0?1.0:(x<0?-1.0:0.0)); }
LumenVal lumen_math_deg(LumenVal v){ return lumen_from_double(as_num(v) * (180.0/3.14159265358979311600)); }
LumenVal lumen_math_rad(LumenVal v){ return lumen_from_double(as_num(v) * (3.14159265358979311600/180.0)); }
LumenVal lumen_math_isnan(LumenVal v){ double x=as_num(v); return lumen_bool(x != x); }
LumenVal lumen_math_isinf(LumenVal v){ double x=as_num(v); return lumen_bool(x > 1.7976931348623157e308 || x < -1.7976931348623157e308); }

LumenVal lumen_math_pi(void){ return lumen_from_double(3.14159265358979311600); }
LumenVal lumen_math_e(void){ return lumen_from_double(2.71828182845904509080); }
LumenVal lumen_math_tau(void){ return lumen_from_double(6.28318530717958623200); }
LumenVal lumen_math_inf(void){ return lumen_from_double(1.0/0.0); }

LumenVal lumen_math_gcd(LumenVal a, LumenVal b){
    int64_t x = (int64_t)as_num(a); if (x < 0) x = -x;
    int64_t y = (int64_t)as_num(b); if (y < 0) y = -y;
    while (y) { int64_t t = x % y; x = y; y = t; }
    return lumen_from_int(x);
}

LumenVal lumen_math_lcm(LumenVal a, LumenVal b){
    int64_t x = (int64_t)as_num(a); if (x < 0) x = -x;
    int64_t y = (int64_t)as_num(b); if (y < 0) y = -y;
    if (x == 0 || y == 0) return lumen_from_int(0);
    int64_t g = x, h = y; while (h) { int64_t t = g % h; g = h; h = t; }
    return lumen_from_int((x / g) * y);
}

LumenVal lumen_math_factorial(LumenVal n){
    int64_t k = (int64_t)as_num(n);
    if (k < 0) return lumen_from_int(0);
    int64_t r = 1; for (int64_t i = 2; i <= k; i++) r *= i;
    return lumen_from_int(r);
}

LumenVal lumen_math_fmod(LumenVal a, LumenVal b){ return lumen_from_double(fmod(as_num(a), as_num(b))); }
LumenVal lumen_math_copysign(LumenVal a, LumenVal b){ return lumen_from_double(copysign(as_num(a), as_num(b))); }
LumenVal lumen_math_log1p(LumenVal v){ return lumen_from_double(log1p(as_num(v))); }
LumenVal lumen_math_expm1(LumenVal v){ return lumen_from_double(expm1(as_num(v))); }
LumenVal lumen_math_isfinite(LumenVal v){ double x=as_num(v); return lumen_bool(x==x && x<1.7976931348623157e308 && x>-1.7976931348623157e308); }

const char *lumen_cstr(LumenVal v) {
    if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        if (o->kind == OBJ_STR) return ((LumenStr*)o)->data;
    }
    return "";
}

static LumenCBuf *as_cbuf(LumenVal v, const char *who) {
    if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        if (o->kind == OBJ_CBUF) return (LumenCBuf *)o;
    }
    char m[64];
    snprintf(m, sizeof(m), "%s: expected a cbuf", who);
    lumen_die(m);
}

LumenVal lumen_cbuf(LumenVal size_v) {
    int64_t n = lumen_ffi_argint(size_v);
    if (n < 0) lumen_die("cbuf: size must be >= 0");
    LumenCBuf *b = xalloc(sizeof(LumenCBuf));
    gc_register(b);
    b->kind = OBJ_CBUF;
    b->rc = 1;
    b->len = (size_t)n;
    b->data = xalloc((size_t)n ? (size_t)n : 1);
    memset(b->data, 0, (size_t)n);
    return box_ptr(b);
}

LumenVal lumen_cbuf_addr(LumenVal v) {
    LumenCBuf *b = as_cbuf(v, "cbuf_addr");
    return lumen_from_int((int64_t)(intptr_t)b->data);
}

LumenVal lumen_cbuf_len(LumenVal v) {
    LumenCBuf *b = as_cbuf(v, "cbuf_len");
    return lumen_from_int((int64_t)b->len);
}

static void cbuf_check(LumenCBuf *b, int64_t off, size_t width, const char *who) {
    if (off < 0 || (size_t)off + width > b->len) {
        char m[80];
        snprintf(m, sizeof(m), "%s: offset %lld out of bounds (size %zu)",
                 who, (long long)off, b->len);
        lumen_die(m);
    }
}

LumenVal lumen_cset_i8(LumenVal v, LumenVal off_v, LumenVal val) {
    LumenCBuf *b = as_cbuf(v, "cset_i8"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 1, "cset_i8"); int8_t x = (int8_t)lumen_ffi_argint(val);
    memcpy(b->data + off, &x, 1); return lumen_nil();
}
LumenVal lumen_cset_i16(LumenVal v, LumenVal off_v, LumenVal val) {
    LumenCBuf *b = as_cbuf(v, "cset_i16"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 2, "cset_i16"); int16_t x = (int16_t)lumen_ffi_argint(val);
    memcpy(b->data + off, &x, 2); return lumen_nil();
}
LumenVal lumen_cset_i32(LumenVal v, LumenVal off_v, LumenVal val) {
    LumenCBuf *b = as_cbuf(v, "cset_i32"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 4, "cset_i32"); int32_t x = (int32_t)lumen_ffi_argint(val);
    memcpy(b->data + off, &x, 4); return lumen_nil();
}
LumenVal lumen_cset_i64(LumenVal v, LumenVal off_v, LumenVal val) {
    LumenCBuf *b = as_cbuf(v, "cset_i64"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 8, "cset_i64"); int64_t x = lumen_ffi_argint(val);
    memcpy(b->data + off, &x, 8); return lumen_nil();
}
LumenVal lumen_cset_ptr(LumenVal v, LumenVal off_v, LumenVal val) {
    LumenCBuf *b = as_cbuf(v, "cset_ptr"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 8, "cset_ptr"); int64_t x = lumen_ffi_argint(val);
    memcpy(b->data + off, &x, 8); return lumen_nil();
}
LumenVal lumen_cset_f32(LumenVal v, LumenVal off_v, LumenVal val) {
    LumenCBuf *b = as_cbuf(v, "cset_f32"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 4, "cset_f32");
    double d; int64_t bits = lumen_ffi_argdouble(val); memcpy(&d, &bits, 8);
    float f = (float)d; memcpy(b->data + off, &f, 4); return lumen_nil();
}
LumenVal lumen_cset_f64(LumenVal v, LumenVal off_v, LumenVal val) {
    LumenCBuf *b = as_cbuf(v, "cset_f64"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 8, "cset_f64");
    int64_t bits = lumen_ffi_argdouble(val); memcpy(b->data + off, &bits, 8);
    return lumen_nil();
}

LumenVal lumen_cget_i8(LumenVal v, LumenVal off_v) {
    LumenCBuf *b = as_cbuf(v, "cget_i8"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 1, "cget_i8"); int8_t x; memcpy(&x, b->data + off, 1);
    return lumen_from_int((int64_t)x);
}
LumenVal lumen_cget_i16(LumenVal v, LumenVal off_v) {
    LumenCBuf *b = as_cbuf(v, "cget_i16"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 2, "cget_i16"); int16_t x; memcpy(&x, b->data + off, 2);
    return lumen_from_int((int64_t)x);
}
LumenVal lumen_cget_i32(LumenVal v, LumenVal off_v) {
    LumenCBuf *b = as_cbuf(v, "cget_i32"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 4, "cget_i32"); int32_t x; memcpy(&x, b->data + off, 4);
    return lumen_from_int((int64_t)x);
}
LumenVal lumen_cget_i64(LumenVal v, LumenVal off_v) {
    LumenCBuf *b = as_cbuf(v, "cget_i64"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 8, "cget_i64"); int64_t x; memcpy(&x, b->data + off, 8);
    return lumen_from_int(x);
}
LumenVal lumen_cget_ptr(LumenVal v, LumenVal off_v) {
    LumenCBuf *b = as_cbuf(v, "cget_ptr"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 8, "cget_ptr"); int64_t x; memcpy(&x, b->data + off, 8);
    return lumen_from_int(x);
}
LumenVal lumen_cget_f32(LumenVal v, LumenVal off_v) {
    LumenCBuf *b = as_cbuf(v, "cget_f32"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 4, "cget_f32"); float f; memcpy(&f, b->data + off, 4);
    return lumen_from_double((double)f);
}
LumenVal lumen_cget_f64(LumenVal v, LumenVal off_v) {
    LumenCBuf *b = as_cbuf(v, "cget_f64"); int64_t off = lumen_ffi_argint(off_v);
    cbuf_check(b, off, 8, "cget_f64"); double d; memcpy(&d, b->data + off, 8);
    return lumen_from_double(d);
}

LumenVal lumen_peek_i64(LumenVal addr_v) {
    int64_t x; memcpy(&x, (void *)(intptr_t)lumen_ffi_argint(addr_v), 8);
    return lumen_from_int(x);
}
LumenVal lumen_poke_i64(LumenVal addr_v, LumenVal val) {
    int64_t x = lumen_ffi_argint(val);
    memcpy((void *)(intptr_t)lumen_ffi_argint(addr_v), &x, 8);
    return lumen_nil();
}
LumenVal lumen_peek_i32(LumenVal addr_v) {
    int32_t x; memcpy(&x, (void *)(intptr_t)lumen_ffi_argint(addr_v), 4);
    return lumen_from_int((int64_t)x);
}
LumenVal lumen_poke_i32(LumenVal addr_v, LumenVal val) {
    int32_t x = (int32_t)lumen_ffi_argint(val);
    memcpy((void *)(intptr_t)lumen_ffi_argint(addr_v), &x, 4);
    return lumen_nil();
}

LumenVal lumen_str_ptr(LumenVal v) {
    if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        if (o->kind == OBJ_STR) return lumen_from_int((int64_t)(intptr_t)((LumenStr *)o)->data);
    }
    return lumen_from_int(0);
}

static int hexnib(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}
LumenVal lumen_guid(LumenVal s_v) {
    const char *s = lumen_cstr(s_v);

    unsigned char nib[32];
    int n = 0;
    for (const char *p = s; *p && n < 32; p++) {
        int h = hexnib(*p);
        if (h >= 0) nib[n++] = (unsigned char)h;
    }
    if (n != 32) lumen_die("guid: expected 32 hex digits");
    unsigned char b[16];
    for (int i = 0; i < 16; i++) b[i] = (unsigned char)((nib[i * 2] << 4) | nib[i * 2 + 1]);

    LumenCBuf *cb;
    LumenVal cbv = lumen_cbuf(lumen_from_int(16));
    cb = (LumenCBuf *)unbox_ptr(cbv);
    unsigned char *d = cb->data;
    d[0]=b[3]; d[1]=b[2]; d[2]=b[1]; d[3]=b[0];
    d[4]=b[5]; d[5]=b[4];
    d[6]=b[7]; d[7]=b[6];
    for (int i = 8; i < 16; i++) d[i] = b[i];
    return cbv;
}

LumenVal lumen_os_read(LumenVal path) {
    FILE *f = fopen(lumen_cstr(path), "rb");
    if (!f) return lumen_nil();
    if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return lumen_nil(); }
    long n = ftell(f);
    if (n < 0) { fclose(f); return lumen_nil(); }
    rewind(f);
    char *buf = xalloc((size_t)n + 1);
    size_t got = fread(buf, 1, (size_t)n, f);
    fclose(f);
    buf[got] = '\0';
    LumenVal r = lumen_str_new(buf);
    xfree(buf);
    return r;
}

static LumenVal os_write_mode(LumenVal path, LumenVal text, const char *mode) {
    FILE *f = fopen(lumen_cstr(path), mode);
    if (!f) return lumen_false();
    const char *s = lumen_cstr(text);
    size_t len = strlen(s);
    size_t wrote = fwrite(s, 1, len, f);
    int ok = (fclose(f) == 0) && (wrote == len);
    return lumen_bool(ok);
}

LumenVal lumen_os_write(LumenVal path, LumenVal text) { return os_write_mode(path, text, "wb"); }

LumenVal lumen_os_append(LumenVal path, LumenVal text) { return os_write_mode(path, text, "ab"); }

LumenVal lumen_os_exists(LumenVal path) {
    struct stat st;
    return lumen_bool(stat(lumen_cstr(path), &st) == 0);
}

LumenVal lumen_os_is_file(LumenVal path) {
    struct stat st;
    if (stat(lumen_cstr(path), &st) != 0) return lumen_false();
    return lumen_bool((st.st_mode & S_IFMT) == S_IFREG);
}

LumenVal lumen_os_is_dir(LumenVal path) {
    struct stat st;
    if (stat(lumen_cstr(path), &st) != 0) return lumen_false();
    return lumen_bool((st.st_mode & S_IFMT) == S_IFDIR);
}

LumenVal lumen_os_remove(LumenVal path) { return lumen_bool(remove(lumen_cstr(path)) == 0); }

LumenVal lumen_os_rmdir(LumenVal path) { return lumen_bool(rmdir(lumen_cstr(path)) == 0); }

LumenVal lumen_os_rename(LumenVal from, LumenVal to) {
    return lumen_bool(rename(lumen_cstr(from), lumen_cstr(to)) == 0);
}

LumenVal lumen_os_mkdir(LumenVal path) {
#if defined(_WIN32)
    return lumen_bool(mkdir(lumen_cstr(path)) == 0);
#else
    return lumen_bool(mkdir(lumen_cstr(path), 0777) == 0);
#endif
}

static int os_cmp_str(const void *a, const void *b) {
    return strcmp(*(const char *const *)a, *(const char *const *)b);
}

LumenVal lumen_os_listdir(LumenVal path) {
    DIR *d = opendir(lumen_cstr(path));
    if (!d) return lumen_nil();
    char **names = NULL;
    size_t n = 0, cap = 0;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        if (n == cap) { cap = cap ? cap * 2 : 8; names = realloc(names, cap * sizeof(char *)); }
        names[n] = xalloc(strlen(e->d_name) + 1);
        strcpy(names[n], e->d_name);
        n++;
    }
    closedir(d);
    qsort(names, n, sizeof(char *), os_cmp_str);
    LumenVal list = lumen_list_new((int64_t)n);
    for (size_t i = 0; i < n; i++) { lumen_list_push(list, lumen_str_new(names[i])); xfree(names[i]); }
    free(names);
    return list;
}

LumenVal lumen_os_getenv(LumenVal name) {
    const char *v = getenv(lumen_cstr(name));
    return v ? lumen_str_new(v) : lumen_nil();
}

LumenVal lumen_os_setenv(LumenVal name, LumenVal val) {
    const char *k = lumen_cstr(name);
    const char *v = lumen_cstr(val);
    size_t n = strlen(k) + strlen(v) + 2;
    char *buf = xalloc(n);
    snprintf(buf, n, "%s=%s", k, v);
    int rc = putenv(buf);
    return lumen_bool(rc == 0);
}

LumenVal lumen_os_cwd(void) {
    char buf[4096];
    if (getcwd(buf, sizeof(buf))) return lumen_str_new(buf);
    return lumen_nil();
}

LumenVal lumen_os_time(void) {
    return lumen_from_int((int64_t)time(NULL));
}

LumenVal lumen_os_clock(void) {
    return lumen_from_int((int64_t)(clock() * 1000 / CLOCKS_PER_SEC));
}

LumenVal lumen_os_getpid(void) {
    return lumen_from_int((int64_t)getpid());
}

LumenVal lumen_os_sep(void) {
#ifdef _WIN32
    return lumen_str_new("\\");
#else
    return lumen_str_new("/");
#endif
}

LumenVal lumen_os_platform(void) {
#ifdef _WIN32
    return lumen_str_new("windows");
#elif defined(__APPLE__)
    return lumen_str_new("macos");
#else
    return lumen_str_new("linux");
#endif
}

LumenVal lumen_os_system(LumenVal cmd) {
    int rc = system(lumen_cstr(cmd));
#ifndef _WIN32

    if (rc != -1 && WIFEXITED(rc)) rc = WEXITSTATUS(rc);
#endif
    return lumen_from_int((int64_t)rc);
}

LumenVal lumen_os_exec(LumenVal cmd) {
    FILE *p = popen(lumen_cstr(cmd), "r");
    if (!p) return lumen_nil();
    size_t cap = 256, len = 0;
    char *buf = xalloc(cap);
    size_t n;
    char tmp[4096];
    while ((n = fread(tmp, 1, sizeof(tmp), p)) > 0) {
        if (len + n + 1 > cap) {
            while (len + n + 1 > cap) cap *= 2;
            buf = realloc(buf, cap);
        }
        memcpy(buf + len, tmp, n);
        len += n;
    }
    buf[len] = '\0';
    pclose(p);
    LumenVal s = lumen_str_new(buf);
    xfree(buf);
    return s;
}

LumenVal lumen_os_exit(LumenVal code) {
    int c = lumen_is_int(code) ? (int)lumen_to_int(code) : 0;
    exit(c);
}

static int lumen_argc = 0;
static char **lumen_argv = NULL;
void lumen_set_args(int argc, char **argv) {
    lumen_argc = argc;
    lumen_argv = argv;
}

LumenVal lumen_os_args(void) {
    LumenVal list = lumen_list_new((int64_t)(lumen_argc > 0 ? lumen_argc : 1));
    for (int i = 0; i < lumen_argc; i++) {
        lumen_list_push(list, lumen_str_new(lumen_argv[i] ? lumen_argv[i] : ""));
    }
    return list;
}

/* net: Winsock2 TCP/UDP sockets mirroring src/net.rs. A socket is an int handle
 * (-1 = error). recv/recvfrom return text (or nil on error); recvfrom returns a
 * map {data, host, port}. Windows-only (matches interp). 
 * DO NOT TOUCH IF YOU DO NOT KNOW WHAT YOU ARE DOING!!!
 * DO NOT RENAME THE FUNCTIONS, THIS FUNCTIONS ARE NAMED LIKE THIS SPECFICALLY,
 * FOR COLLISION WITHIN OTHER FUNCTIONS!!!
 */

LumenVal lumen_map_new(void);
void lumen_map_set(LumenVal mv, LumenVal key, LumenVal val);

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>

static int net_started = 0;
static void net_start(void) {
    if (!net_started) {
        WSADATA d;
        WSAStartup(MAKEWORD(2, 2), &d);
        net_started = 1;
    }
}

/* Fill a sockaddr_in for host:port. host "" = INADDR_ANY. Returns 1 on success. */
static int net_addr(const char *host, int port, struct sockaddr_in *sa) {
    memset(sa, 0, sizeof(*sa));
    sa->sin_family = AF_INET;
    sa->sin_port = htons((unsigned short)port);
    if (!host || !host[0]) {
        sa->sin_addr.s_addr = INADDR_ANY;
        return 1;
    }
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    if (getaddrinfo(host, NULL, &hints, &res) != 0 || !res) return 0;
    struct sockaddr_in *r = (struct sockaddr_in *)res->ai_addr;
    sa->sin_addr = r->sin_addr;
    freeaddrinfo(res);
    return 1;
}

static LumenVal net_ip_string(struct in_addr a) {
    unsigned char *o = (unsigned char *)&a.s_addr;
    char buf[32];
    snprintf(buf, sizeof buf, "%u.%u.%u.%u", o[0], o[1], o[2], o[3]);
    return lumen_str_new(buf);
}

LumenVal lumen_net_listen(LumenVal host, LumenVal port) {
    net_start();
    SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (s == INVALID_SOCKET) return lumen_from_int(-1);
    int one = 1;
    setsockopt(s, SOL_SOCKET, SO_REUSEADDR, (char *)&one, sizeof one);
    struct sockaddr_in sa;
    if (!net_addr(lumen_cstr(host), (int)lumen_to_int(port), &sa)
        || bind(s, (struct sockaddr *)&sa, sizeof sa) == SOCKET_ERROR
        || listen(s, 128) == SOCKET_ERROR) {
        closesocket(s);
        return lumen_from_int(-1);
    }
    return lumen_from_int((int64_t)s);
}

LumenVal lumen_net_accept(LumenVal sock) {
    SOCKET c = accept((SOCKET)lumen_to_int(sock), NULL, NULL);
    return lumen_from_int(c == INVALID_SOCKET ? -1 : (int64_t)c);
}

LumenVal lumen_net_connect(LumenVal host, LumenVal port) {
    net_start();
    SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (s == INVALID_SOCKET) return lumen_from_int(-1);
    struct sockaddr_in sa;
    if (!net_addr(lumen_cstr(host), (int)lumen_to_int(port), &sa)
        || connect(s, (struct sockaddr *)&sa, sizeof sa) == SOCKET_ERROR) {
        closesocket(s);
        return lumen_from_int(-1);
    }
    return lumen_from_int((int64_t)s);
}

LumenVal lumen_net_udp(LumenVal host, LumenVal port) {
    net_start();
    SOCKET s = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if (s == INVALID_SOCKET) return lumen_from_int(-1);
    const char *h = lumen_cstr(host);
    int p = (int)lumen_to_int(port);
    struct sockaddr_in sa;
    if (!net_addr(h, p, &sa)) { closesocket(s); return lumen_from_int(-1); }
    if ((h[0] || p != 0) && bind(s, (struct sockaddr *)&sa, sizeof sa) == SOCKET_ERROR) {
        closesocket(s);
        return lumen_from_int(-1);
    }
    return lumen_from_int((int64_t)s);
}

LumenVal lumen_net_send(LumenVal sock, LumenVal data) {
    const char *p = lumen_cstr(data);
    int n = send((SOCKET)lumen_to_int(sock), p, (int)strlen(p), 0);
    return lumen_from_int((int64_t)n);
}

LumenVal lumen_net_recv(LumenVal sock, LumenVal max) {
    int m = (int)lumen_to_int(max);
    if (m < 0) m = 0;
    char *buf = xalloc((size_t)m + 1);
    int n = recv((SOCKET)lumen_to_int(sock), buf, m, 0);
    if (n < 0) { xfree(buf); return lumen_nil(); }
    buf[n] = '\0';
    LumenVal r = lumen_str_new(buf);
    xfree(buf);
    return r;
}

LumenVal lumen_net_sendto(LumenVal sock, LumenVal data, LumenVal host, LumenVal port) {
    struct sockaddr_in sa;
    if (!net_addr(lumen_cstr(host), (int)lumen_to_int(port), &sa)) return lumen_from_int(-1);
    const char *p = lumen_cstr(data);
    int n = sendto((SOCKET)lumen_to_int(sock), p, (int)strlen(p), 0,
                   (struct sockaddr *)&sa, sizeof sa);
    return lumen_from_int((int64_t)n);
}

LumenVal lumen_net_recvfrom(LumenVal sock, LumenVal max) {
    int m = (int)lumen_to_int(max);
    if (m < 0) m = 0;
    char *buf = xalloc((size_t)m + 1);
    struct sockaddr_in from;
    int flen = sizeof from;
    memset(&from, 0, sizeof from);
    int n = recvfrom((SOCKET)lumen_to_int(sock), buf, m, 0, (struct sockaddr *)&from, &flen);
    if (n < 0) { xfree(buf); return lumen_nil(); }
    buf[n] = '\0';
    LumenVal mp = lumen_map_new();
    lumen_map_set(mp, lumen_str_new("data"), lumen_str_new(buf));
    lumen_map_set(mp, lumen_str_new("host"), net_ip_string(from.sin_addr));
    lumen_map_set(mp, lumen_str_new("port"), lumen_from_int((int64_t)ntohs(from.sin_port)));
    xfree(buf);
    return mp;
}

LumenVal lumen_net_close(LumenVal sock) {
    closesocket((SOCKET)lumen_to_int(sock));
    return lumen_nil();
}

LumenVal lumen_net_shutdown(LumenVal sock, LumenVal how) {
    return lumen_from_int((int64_t)shutdown((SOCKET)lumen_to_int(sock), (int)lumen_to_int(how)));
}

LumenVal lumen_net_set_timeout(LumenVal sock, LumenVal ms) {
    SOCKET s = (SOCKET)lumen_to_int(sock);
    DWORD t = (DWORD)lumen_to_int(ms);
    int r1 = setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, (char *)&t, sizeof t);
    int r2 = setsockopt(s, SOL_SOCKET, SO_SNDTIMEO, (char *)&t, sizeof t);
    return lumen_from_int((r1 == 0 && r2 == 0) ? 0 : -1);
}

LumenVal lumen_net_set_blocking(LumenVal sock, LumenVal blocking) {
    u_long mode = lumen_to_int(blocking) ? 0 : 1;
    return lumen_from_int((int64_t)ioctlsocket((SOCKET)lumen_to_int(sock), FIONBIO, &mode));
}

LumenVal lumen_net_set_opt(LumenVal sock, LumenVal name, LumenVal val) {
    SOCKET s = (SOCKET)lumen_to_int(sock);
    const char *n = lumen_cstr(name);
    int v = (int)lumen_to_int(val);
    int level = SOL_SOCKET, opt = -1;
    if (!strcmp(n, "reuseaddr")) opt = SO_REUSEADDR;
    else if (!strcmp(n, "keepalive")) opt = SO_KEEPALIVE;
    else if (!strcmp(n, "broadcast")) opt = SO_BROADCAST;
    else if (!strcmp(n, "sndbuf")) opt = SO_SNDBUF;
    else if (!strcmp(n, "rcvbuf")) opt = SO_RCVBUF;
    else if (!strcmp(n, "nodelay")) { level = IPPROTO_TCP; opt = TCP_NODELAY; }
    else return lumen_from_int(-1);
    return lumen_from_int((int64_t)setsockopt(s, level, opt, (char *)&v, sizeof v));
}

LumenVal lumen_net_poll(LumenVal sock, LumenVal ms) {
    SOCKET s = (SOCKET)lumen_to_int(sock);
    int64_t t = lumen_to_int(ms);
    fd_set rd, wr;
    FD_ZERO(&rd); FD_ZERO(&wr);
    FD_SET(s, &rd); FD_SET(s, &wr);
    struct timeval tv;
    tv.tv_sec = (long)(t / 1000);
    tv.tv_usec = (long)((t % 1000) * 1000);
    int r = select(0, &rd, &wr, NULL, t < 0 ? NULL : &tv);
    if (r < 0) return lumen_from_int(-1);
    int64_t mask = 0;
    if (FD_ISSET(s, &rd)) mask |= 1;
    if (FD_ISSET(s, &wr)) mask |= 2;
    return lumen_from_int(mask);
}

LumenVal lumen_net_resolve(LumenVal host) {
    net_start();
    struct sockaddr_in sa;
    if (!net_addr(lumen_cstr(host), 0, &sa) || sa.sin_addr.s_addr == 0) return lumen_nil();
    return net_ip_string(sa.sin_addr);
}

LumenVal lumen_net_local_port(LumenVal sock) {
    struct sockaddr_in sa;
    int len = sizeof sa;
    if (getsockname((SOCKET)lumen_to_int(sock), (struct sockaddr *)&sa, &len) == SOCKET_ERROR)
        return lumen_from_int(-1);
    return lumen_from_int((int64_t)ntohs(sa.sin_port));
}

LumenVal lumen_net_errno(void) { return lumen_from_int((int64_t)WSAGetLastError()); }
#else
static LumenVal net_unsupported(void) {
    lumen_raise(lumen_str_new("net: only supported on Windows"));
    return lumen_nil();
}
LumenVal lumen_net_listen(LumenVal a, LumenVal b) { (void)a; (void)b; return net_unsupported(); }
LumenVal lumen_net_accept(LumenVal a) { (void)a; return net_unsupported(); }
LumenVal lumen_net_connect(LumenVal a, LumenVal b) { (void)a; (void)b; return net_unsupported(); }
LumenVal lumen_net_udp(LumenVal a, LumenVal b) { (void)a; (void)b; return net_unsupported(); }
LumenVal lumen_net_send(LumenVal a, LumenVal b) { (void)a; (void)b; return net_unsupported(); }
LumenVal lumen_net_recv(LumenVal a, LumenVal b) { (void)a; (void)b; return net_unsupported(); }
LumenVal lumen_net_sendto(LumenVal a, LumenVal b, LumenVal c, LumenVal d) { (void)a; (void)b; (void)c; (void)d; return net_unsupported(); }
LumenVal lumen_net_recvfrom(LumenVal a, LumenVal b) { (void)a; (void)b; return net_unsupported(); }
LumenVal lumen_net_close(LumenVal a) { (void)a; return net_unsupported(); }
LumenVal lumen_net_shutdown(LumenVal a, LumenVal b) { (void)a; (void)b; return net_unsupported(); }
LumenVal lumen_net_set_timeout(LumenVal a, LumenVal b) { (void)a; (void)b; return net_unsupported(); }
LumenVal lumen_net_set_blocking(LumenVal a, LumenVal b) { (void)a; (void)b; return net_unsupported(); }
LumenVal lumen_net_set_opt(LumenVal a, LumenVal b, LumenVal c) { (void)a; (void)b; (void)c; return net_unsupported(); }
LumenVal lumen_net_poll(LumenVal a, LumenVal b) { (void)a; (void)b; return net_unsupported(); }
LumenVal lumen_net_resolve(LumenVal a) { (void)a; return net_unsupported(); }
LumenVal lumen_net_local_port(LumenVal a) { (void)a; return net_unsupported(); }
LumenVal lumen_net_errno(void) { return net_unsupported(); }
#endif

static uint64_t lumen_rng_state = 0x9E3779B97F4A7C15ull;
static uint64_t lumen_splitmix64(void) {
    lumen_rng_state += 0x9E3779B97F4A7C15ull;
    uint64_t z = lumen_rng_state;
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
    return z ^ (z >> 31);
}

LumenVal lumen_rand_seed(LumenVal n) {
    lumen_rng_state = (uint64_t)(lumen_is_int(n) ? lumen_to_int(n) : 0);
    return lumen_nil();
}

LumenVal lumen_rand_int(LumenVal lov, LumenVal hiv) {
    int64_t lo = lumen_is_int(lov) ? lumen_to_int(lov) : 0;
    int64_t hi = lumen_is_int(hiv) ? lumen_to_int(hiv) : 0;
    if (hi < lo) return lumen_from_int(lo);
    uint64_t span = (uint64_t)(hi - lo) + 1ull;
    uint64_t r = lumen_splitmix64() % span;
    return lumen_from_int(lo + (int64_t)r);
}

LumenVal lumen_rand_float(void) {
    uint64_t r = lumen_splitmix64() >> 11;
    return lumen_from_double((double)r / 9007199254740992.0 );
}

LumenVal lumen_time_now(void) {
#ifdef _WIN32

    return lumen_from_int((int64_t)time(NULL) * 1000);
#else
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return lumen_from_int((int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000);
#endif
}

LumenVal lumen_time_format(LumenVal secs) {
    time_t t = (time_t)(int64_t)as_num(secs);
    struct tm tmv;
#ifdef _WIN32
    gmtime_s(&tmv, &t);
#else
    gmtime_r(&t, &tmv);
#endif
    char buf[32];
    strftime(buf, sizeof buf, "%Y-%m-%d %H:%M:%S", &tmv);
    return lumen_str_new(buf);
}

LumenVal lumen_time_sleep(LumenVal ms) {
    int64_t m = (int64_t)as_num(ms);
    if (m > 0) {
#ifdef _WIN32
        Sleep((unsigned long)m);
#else
        struct timespec req;
        req.tv_sec = m / 1000;
        req.tv_nsec = (m % 1000) * 1000000L;
        nanosleep(&req, NULL);
#endif
    }
    return lumen_nil();
}

LumenVal lumen_map_new(void);
void lumen_map_set(LumenVal mv, LumenVal key, LumenVal val);

typedef struct { char *p; size_t len, cap; } JBuf;
static void jb_init(JBuf *b) { b->cap = 64; b->len = 0; b->p = xalloc(b->cap); b->p[0] = 0; }
static void jb_putc(JBuf *b, char c) {
    if (b->len + 2 > b->cap) { b->cap *= 2; b->p = realloc(b->p, b->cap); }
    b->p[b->len++] = c; b->p[b->len] = 0;
}
static void jb_puts(JBuf *b, const char *s) {
    size_t n = strlen(s);
    if (b->len + n + 1 > b->cap) { while (b->len + n + 1 > b->cap) b->cap *= 2; b->p = realloc(b->p, b->cap); }
    memcpy(b->p + b->len, s, n); b->len += n; b->p[b->len] = 0;
}

static void jb_put_json_str(JBuf *b, const char *s) {
    jb_putc(b, '"');
    for (; *s; s++) {
        unsigned char c = (unsigned char)*s;
        if (c == '"') { jb_puts(b, "\\\""); }
        else if (c == '\\') { jb_puts(b, "\\\\"); }
        else if (c == '\n') { jb_puts(b, "\\n"); }
        else if (c == '\t') { jb_puts(b, "\\t"); }
        else if (c == 13) { jb_puts(b, "\\" "r"); }
        else if (c == '\b') { jb_puts(b, "\\b"); }
        else if (c == '\f') { jb_puts(b, "\\f"); }
        else if (c < 0x20) { char u[8]; snprintf(u, sizeof u, "\\u%04x", c); jb_puts(b, u); }
        else { jb_putc(b, (char)c); }
    }
    jb_putc(b, '"');
}

static void json_write(JBuf *b, LumenVal v) {
    char num[512];
    if (lumen_is_int(v)) {
        snprintf(num, sizeof num, "%lld", (long long)lumen_to_int(v));
        jb_puts(b, num);
    } else if (lumen_is_double(v)) {
        fmt_double(lumen_to_double(v), num, sizeof num);
        jb_puts(b, num);
    } else if (lumen_is_bool(v)) {
        jb_puts(b, (v & 1) ? "true" : "false");
    } else if (lumen_is_nil(v)) {
        jb_puts(b, "null");
    } else if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        if (o->kind == OBJ_STR) {
            jb_put_json_str(b, ((LumenStr *)o)->data);
        } else if (o->kind == OBJ_LIST) {
            LumenList *l = (LumenList *)o;
            jb_putc(b, '[');
            for (size_t i = 0; i < l->len; i++) { if (i) jb_putc(b, ','); json_write(b, l->items[i]); }
            jb_putc(b, ']');
        } else if (o->kind == OBJ_MAP) {
            LumenMap *m = (LumenMap *)o;
            jb_putc(b, '{');
            for (size_t i = 0; i < m->len; i++) {
                if (i) jb_putc(b, ',');

                LumenVal ks = lumen_to_str(m->keys[i]);
                jb_put_json_str(b, ((LumenStr *)unbox_ptr(ks))->data);
                jb_putc(b, ':');
                json_write(b, m->vals[i]);
            }
            jb_putc(b, '}');
        } else {
            jb_puts(b, "null");
        }
    } else {
        jb_puts(b, "null");
    }
}

LumenVal lumen_json_stringify(LumenVal v) {
    JBuf b; jb_init(&b);
    json_write(&b, v);
    LumenVal s = lumen_str_new(b.p);
    xfree(b.p);
    return s;
}

typedef struct { const char *s; int ok; } JP;
static void jp_skip_ws(JP *p) {
    while (*p->s == ' ' || *p->s == '\t' || *p->s == '\n' || *p->s == (char)13) p->s++;
}
static LumenVal jp_value(JP *p);

static LumenVal jp_string(JP *p) {

    p->s++;
    JBuf b; jb_init(&b);
    while (*p->s && *p->s != '"') {
        char c = *p->s++;
        if (c == '\\') {
            char e = *p->s++;
            switch (e) {
                case '"': jb_putc(&b, '"'); break;
                case '\\': jb_putc(&b, '\\'); break;
                case '/': jb_putc(&b, '/'); break;
                case 'n': jb_putc(&b, '\n'); break;
                case 't': jb_putc(&b, '\t'); break;
                case 'r': jb_putc(&b, (char)13); break;
                case 'b': jb_putc(&b, '\b'); break;
                case 'f': jb_putc(&b, '\f'); break;
                case 'u': {

                    char hex[5] = {0};
                    for (int i = 0; i < 4 && p->s[i]; i++) hex[i] = p->s[i];
                    p->s += 4;
                    unsigned cp = (unsigned)strtoul(hex, NULL, 16);
                    if (cp < 0x80) jb_putc(&b, (char)cp);
                    else if (cp < 0x800) {
                        jb_putc(&b, (char)(0xC0 | (cp >> 6)));
                        jb_putc(&b, (char)(0x80 | (cp & 0x3F)));
                    } else {
                        jb_putc(&b, (char)(0xE0 | (cp >> 12)));
                        jb_putc(&b, (char)(0x80 | ((cp >> 6) & 0x3F)));
                        jb_putc(&b, (char)(0x80 | (cp & 0x3F)));
                    }
                    break;
                }
                default: jb_putc(&b, e); break;
            }
        } else {
            jb_putc(&b, c);
        }
    }
    if (*p->s != '"') { p->ok = 0; xfree(b.p); return lumen_nil(); }
    p->s++;
    LumenVal s = lumen_str_new(b.p);
    xfree(b.p);
    return s;
}

static LumenVal jp_number(JP *p) {
    const char *start = p->s;
    int is_float = 0;
    if (*p->s == '-') p->s++;
    while ((*p->s >= '0' && *p->s <= '9')) p->s++;
    if (*p->s == '.') { is_float = 1; p->s++; while (*p->s >= '0' && *p->s <= '9') p->s++; }
    if (*p->s == 'e' || *p->s == 'E') {
        is_float = 1; p->s++;
        if (*p->s == '+' || *p->s == '-') p->s++;
        while (*p->s >= '0' && *p->s <= '9') p->s++;
    }
    char tmp[64];
    size_t n = (size_t)(p->s - start);
    if (n >= sizeof tmp) n = sizeof tmp - 1;
    memcpy(tmp, start, n); tmp[n] = 0;
    if (is_float) return lumen_from_double(strtod(tmp, NULL));
    return lumen_from_int((int64_t)strtoll(tmp, NULL, 10));
}

static LumenVal jp_array(JP *p) {
    p->s++;
    LumenVal list = lumen_list_new(4);
    jp_skip_ws(p);
    if (*p->s == ']') { p->s++; return list; }
    for (;;) {
        LumenVal el = jp_value(p);
        if (!p->ok) return lumen_nil();
        lumen_list_push(list, el);
        jp_skip_ws(p);
        if (*p->s == ',') { p->s++; jp_skip_ws(p); continue; }
        if (*p->s == ']') { p->s++; break; }
        p->ok = 0; return lumen_nil();
    }
    return list;
}

static LumenVal jp_object(JP *p) {
    p->s++;
    LumenVal map = lumen_map_new();
    jp_skip_ws(p);
    if (*p->s == '}') { p->s++; return map; }
    for (;;) {
        jp_skip_ws(p);
        if (*p->s != '"') { p->ok = 0; return lumen_nil(); }
        LumenVal key = jp_string(p);
        if (!p->ok) return lumen_nil();
        jp_skip_ws(p);
        if (*p->s != ':') { p->ok = 0; return lumen_nil(); }
        p->s++;
        LumenVal val = jp_value(p);
        if (!p->ok) return lumen_nil();
        lumen_map_set(map, key, val);
        jp_skip_ws(p);
        if (*p->s == ',') { p->s++; continue; }
        if (*p->s == '}') { p->s++; break; }
        p->ok = 0; return lumen_nil();
    }
    return map;
}

static LumenVal jp_value(JP *p) {
    jp_skip_ws(p);
    char c = *p->s;
    if (c == '"') return jp_string(p);
    if (c == '{') return jp_object(p);
    if (c == '[') return jp_array(p);
    if (c == '-' || (c >= '0' && c <= '9')) return jp_number(p);
    if (strncmp(p->s, "true", 4) == 0)  { p->s += 4; return lumen_bool(1); }
    if (strncmp(p->s, "false", 5) == 0) { p->s += 5; return lumen_bool(0); }
    if (strncmp(p->s, "null", 4) == 0)  { p->s += 4; return lumen_nil(); }
    p->ok = 0;
    return lumen_nil();
}

LumenVal lumen_json_parse(LumenVal sv) {
    JP p = { lumen_cstr(sv), 1 };
    LumenVal v = jp_value(&p);
    if (!p.ok) return lumen_nil();
    jp_skip_ws(&p);
    if (*p.s != 0) return lumen_nil();
    return v;
}

int64_t lumen_ffi_argint(LumenVal v) {
    if (lumen_is_int(v)) return lumen_to_int(v);
    if (lumen_is_bool(v)) return (v & 1);
    if (lumen_is_nil(v)) return 0;
    if (lumen_is_double(v)) return (int64_t)lumen_to_double(v);
    if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);

        if (o->kind == OBJ_CBUF) return (int64_t)(intptr_t)((LumenCBuf *)o)->data;
        return (int64_t)(intptr_t)lumen_cstr(v);
    }
    return 0;
}

int64_t lumen_ffi_argdouble(LumenVal v) {
    // FFI float arg: coerce any numeric Lumen value to a double, then return its
    // raw bits so the caller/asm can place it in an xmm register unchanged.
    double d;
    if (lumen_is_double(v)) d = lumen_to_double(v);
    else if (lumen_is_int(v)) d = (double)lumen_to_int(v);
    else if (lumen_is_bool(v)) d = (double)(v & 1);
    else d = 0.0;
    int64_t bits;
    memcpy(&bits, &d, 8);
    return bits;
}

#if defined(__GNUC__) || defined(__clang__)
// Win64 COM call trampoline. Marshals a flat int64 arg array into the Microsoft
// x64 calling convention to invoke a virtual method by raw function pointer:
//   - obj is passed as the implicit `this` in the first integer slot (rcx),
//   - the first 3 user args go in rdx/r8/r9 (mirrored into xmm1-3 in case the
//     callee expects floating-point), and any beyond that are spilled to the
//     stack above the 32-byte shadow space, keeping rsp 16-byte aligned,
//   - integer return comes back in rax; the xmm0 return is copied out via out_xmm
//     so the C caller can reinterpret it as a double when needed.
extern int64_t lumen_com_trampoline(void *fnptr, void *obj, int64_t nargs,
                                    const int64_t *args, int64_t *out_xmm);
__asm__(
    ".globl lumen_com_trampoline\n"
    "lumen_com_trampoline:\n"

    "    pushq %rbp\n"
    "    movq %rsp, %rbp\n"
    "    pushq %rsi\n"
    "    pushq %rdi\n"
    "    pushq %rbx\n"
    "    movq %rcx, %rax\n"
    "    movq %rdx, %rsi\n"
    "    movq %r8, %rbx\n"
    "    movq %r9, %rdi\n"
    "    movq 48(%rbp), %r10\n"
    "    pushq %r10\n"

    "    movq %rbx, %r11\n"
    "    subq $3, %r11\n"
    "    jg 1f\n"
    "    xorq %r11, %r11\n"
    "1:  leaq 32(,%r11,8), %rcx\n"
    "    addq $15, %rcx\n"
    "    andq $-16, %rcx\n"
    "    subq %rcx, %rsp\n"
    "    andq $-16, %rsp\n"

    "    movq $3, %r10\n"
    "2:  cmpq %rbx, %r10\n"
    "    jge 3f\n"
    "    movq (%rdi,%r10,8), %r11\n"
    "    leaq -3(%r10), %rcx\n"
    "    movq %r11, 32(%rsp,%rcx,8)\n"
    "    incq %r10\n"
    "    jmp 2b\n"
    "3:\n"

    "    movq %rsi, %rcx\n"
    "    cmpq $1, %rbx\n"
    "    jl 9f\n"
    "    movq (%rdi), %rdx\n"
    "    movq (%rdi), %xmm1\n"
    "    cmpq $2, %rbx\n"
    "    jl 9f\n"
    "    movq 8(%rdi), %r8\n"
    "    movq 8(%rdi), %xmm2\n"
    "    cmpq $3, %rbx\n"
    "    jl 9f\n"
    "    movq 16(%rdi), %r9\n"
    "    movq 16(%rdi), %xmm3\n"
    "9:  call *%rax\n"

    "    leaq -32(%rbp), %rsp\n"
    "    popq %r10\n"
    "    movq %xmm0, (%r10)\n"
    "    popq %rbx\n"
    "    popq %rdi\n"
    "    popq %rsi\n"
    "    popq %rbp\n"
    "    ret\n"
);
#endif

LumenVal lumen_com_vcall(LumenVal obj_v, LumenVal slot_v, LumenVal args_v, LumenVal retkind_v) {
    void *obj = (void *)(intptr_t)lumen_ffi_argint(obj_v);
    int64_t slot = lumen_ffi_argint(slot_v);
    int64_t retkind = lumen_ffi_argint(retkind_v);
    if (!obj) lumen_die("vcall: null object");
    if (slot < 0) lumen_die("vcall: negative slot");

    const int64_t *args = NULL;
    int64_t nargs = 0;
    if (lumen_is_ptr(args_v)) {
        LumenObj *o = unbox_ptr(args_v);
        if (o->kind == OBJ_CBUF) {
            LumenCBuf *b = (LumenCBuf *)o;
            args = (const int64_t *)b->data;
            nargs = (int64_t)(b->len / 8);
        }
    }
    // A COM object's first word points to its vtable; the method lives at [slot].
    void *vtbl = *(void **)obj;
    void *fnptr = ((void **)vtbl)[slot];

    int64_t out_xmm = 0;
    int64_t rax = lumen_com_trampoline(fnptr, obj, nargs, args, &out_xmm);
    if (retkind == 1) {
        double d;
        memcpy(&d, &out_xmm, 8);
        return lumen_from_double(d);
    }
    return lumen_from_int(rax);
}

LumenVal lumen_list_pop(LumenVal lst) {
    LumenList *l = unbox_ptr(lst);
    if (l->len == 0) { lumen_die("pop from empty list"); }
    return l->items[--l->len];
}
void lumen_list_insert(LumenVal lst, int64_t idx, LumenVal v) {
    LumenList *l = unbox_ptr(lst);
    if (idx < 0 || (size_t)idx > l->len) { lumen_die("insert index out of range"); }
    if (l->len == l->cap) { l->cap *= 2; l->items = realloc(l->items, l->cap * sizeof(LumenVal)); }
    for (size_t i = l->len; i > (size_t)idx; i--) l->items[i] = l->items[i-1];
    l->items[idx] = v;
    l->len++;
}
LumenVal lumen_list_contains(LumenVal lst, LumenVal v);

LumenVal lumen_sum(LumenVal lst) {
    LumenList *l = unbox_ptr(lst);
    int all_int = 1;
    for (size_t i = 0; i < l->len; i++) if (!lumen_is_int(l->items[i])) { all_int = 0; break; }
    if (all_int) {
        int64_t s = 0;
        for (size_t i = 0; i < l->len; i++) s += lumen_to_int(l->items[i]);
        return lumen_from_int(s);
    }
    double s = 0;
    for (size_t i = 0; i < l->len; i++) s += as_num(l->items[i]);
    return lumen_from_double(s);
}
LumenVal lumen_min(LumenVal lst) {
    LumenList *l = unbox_ptr(lst);
    if (l->len == 0) { lumen_die("min of empty list"); }
    LumenVal best = l->items[0];
    for (size_t i = 1; i < l->len; i++)
        if (as_num(l->items[i]) < as_num(best)) best = l->items[i];
    return best;
}
LumenVal lumen_max(LumenVal lst) {
    LumenList *l = unbox_ptr(lst);
    if (l->len == 0) { lumen_die("max of empty list"); }
    LumenVal best = l->items[0];
    for (size_t i = 1; i < l->len; i++)
        if (as_num(l->items[i]) > as_num(best)) best = l->items[i];
    return best;
}
LumenVal lumen_abs(LumenVal v) {
    if (lumen_is_int(v)) { int64_t n = lumen_to_int(v); return lumen_from_int(n < 0 ? -n : n); }
    return lumen_from_double(fabs(as_num(v)));
}

LumenVal lumen_round(LumenVal v) {
    if (lumen_is_int(v)) return v;
    return lumen_from_int((int64_t)round(as_num(v)));
}

void lumen_assert(LumenVal cond) {
    if (!lumen_truthy(cond)) { lumen_die("assertion failed"); }
}

LumenVal lumen_type(LumenVal v) {
    const char *t;
    if (lumen_is_double(v)) t = "f64";
    else if (lumen_is_int(v)) t = "i64";
    else if (lumen_is_bool(v)) t = "bool";
    else if (lumen_is_nil(v)) t = "nil";
    else if (lumen_is_ptr(v)) {
        LumenObj *o = unbox_ptr(v);
        t = o->kind == OBJ_STR ? "str" : o->kind == OBJ_LIST ? "list" : o->kind == OBJ_MAP ? "map" : o->kind == OBJ_FUNC ? "fn" : o->kind == OBJ_CBUF ? "cbuf" : "struct";
    } else t = "unknown";
    return lumen_str_new(t);
}

static LumenStr *as_str(LumenVal v) {
    if (lumen_is_ptr(v)) { LumenObj *o = unbox_ptr(v); if (o->kind == OBJ_STR) return (LumenStr*)o; }
    lumen_die("expected a string");
}
LumenVal lumen_str_upper(LumenVal v) {
    LumenStr *s = as_str(v);
    LumenVal r = lumen_str_new(s->data);
    LumenStr *rs = unbox_ptr(r);
    for (size_t i = 0; i < rs->len; i++) if (rs->data[i] >= 'a' && rs->data[i] <= 'z') rs->data[i] -= 32;
    return r;
}
LumenVal lumen_str_lower(LumenVal v) {
    LumenStr *s = as_str(v);
    LumenVal r = lumen_str_new(s->data);
    LumenStr *rs = unbox_ptr(r);
    for (size_t i = 0; i < rs->len; i++) if (rs->data[i] >= 'A' && rs->data[i] <= 'Z') rs->data[i] += 32;
    return r;
}

static int lumen_is_ws_ch(char c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r';
}
LumenVal lumen_str_trim(LumenVal v) {
    LumenStr *s = as_str(v);
    size_t a = 0, b = s->len;
    while (a < b && lumen_is_ws_ch(s->data[a])) a++;
    while (b > a && lumen_is_ws_ch(s->data[b-1])) b--;
    char *buf = xalloc(b - a + 1);
    memcpy(buf, s->data + a, b - a);
    buf[b - a] = '\0';
    LumenVal r = lumen_str_new(buf);
    xfree(buf);
    return r;
}

LumenVal lumen_str_starts_with(LumenVal v, LumenVal pre) {
    LumenStr *s = as_str(v); LumenStr *p = as_str(pre);
    if (p->len > s->len) return lumen_false();
    return lumen_bool(memcmp(s->data, p->data, p->len) == 0);
}

LumenVal lumen_str_ends_with(LumenVal v, LumenVal suf) {
    LumenStr *s = as_str(v); LumenStr *p = as_str(suf);
    if (p->len > s->len) return lumen_false();
    return lumen_bool(memcmp(s->data + (s->len - p->len), p->data, p->len) == 0);
}
int lumen_str_contains_b(LumenVal hay, LumenVal needle) {
    return strstr(as_str(hay)->data, as_str(needle)->data) != NULL;
}
LumenVal lumen_str_contains(LumenVal hay, LumenVal needle) {
    return lumen_bool(lumen_str_contains_b(hay, needle));
}

LumenVal lumen_str_split(LumenVal v, LumenVal sepv) {
    LumenStr *s = as_str(v);
    LumenStr *sep = as_str(sepv);
    LumenVal out = lumen_list_new(4);
    if (sep->len == 0) { lumen_list_push(out, lumen_str_new(s->data)); return out; }
    const char *start = s->data;
    const char *p;
    while ((p = strstr(start, sep->data)) != NULL) {
        size_t n = (size_t)(p - start);
        char *buf = xalloc(n + 1);
        memcpy(buf, start, n); buf[n] = 0;
        lumen_list_push(out, lumen_str_new(buf));
        free(buf);
        start = p + sep->len;
    }
    lumen_list_push(out, lumen_str_new(start));
    return out;
}

LumenVal lumen_str_find(LumenVal v, LumenVal subv) {
    LumenStr *s = as_str(v);
    LumenStr *sub = as_str(subv);
    if (sub->len == 0) return lumen_from_int(0);
    const char *p = strstr(s->data, sub->data);
    if (p == NULL) return lumen_from_int(-1);
    return lumen_from_int((int64_t)(p - s->data));
}

LumenVal lumen_str_lstrip(LumenVal v) {
    LumenStr *s = as_str(v);
    size_t a = 0, b = s->len;
    while (a < b && lumen_is_ws_ch(s->data[a])) a++;
    char *buf = xalloc(b - a + 1);
    memcpy(buf, s->data + a, b - a);
    buf[b - a] = '\0';
    LumenVal r = lumen_str_new(buf);
    xfree(buf);
    return r;
}

LumenVal lumen_str_rstrip(LumenVal v) {
    LumenStr *s = as_str(v);
    size_t a = 0, b = s->len;
    while (b > a && lumen_is_ws_ch(s->data[b-1])) b--;
    char *buf = xalloc(b - a + 1);
    memcpy(buf, s->data + a, b - a);
    buf[b - a] = '\0';
    LumenVal r = lumen_str_new(buf);
    xfree(buf);
    return r;
}

LumenVal lumen_str_replace(LumenVal v, LumenVal oldv, LumenVal newv) {
    LumenStr *s = as_str(v);
    LumenStr *o = as_str(oldv);
    LumenStr *n = as_str(newv);
    if (o->len == 0) return lumen_str_new(s->data);

    size_t count = 0;
    const char *p = s->data;
    const char *q;
    while ((q = strstr(p, o->data)) != NULL) { count++; p = q + o->len; }
    if (count == 0) return lumen_str_new(s->data);
    size_t total = s->len + count * (n->len - o->len);
    char *buf = xalloc(total + 1);
    size_t pos = 0;
    const char *start = s->data;
    while ((q = strstr(start, o->data)) != NULL) {
        size_t seg = (size_t)(q - start);
        memcpy(buf + pos, start, seg); pos += seg;
        memcpy(buf + pos, n->data, n->len); pos += n->len;
        start = q + o->len;
    }

    size_t rest = s->len - (size_t)(start - s->data);
    memcpy(buf + pos, start, rest); pos += rest;
    buf[pos] = '\0';
    LumenVal r = lumen_str_new(buf);
    xfree(buf);
    return r;
}

LumenVal lumen_str_repeat(LumenVal v, LumenVal nv) {
    LumenStr *s = as_str(v);
    int64_t n = lumen_to_int(nv);
    if (n < 0) n = 0;
    size_t count = (size_t)n;
    size_t total = s->len * count;
    char *buf = xalloc(total + 1);
    for (size_t i = 0; i < count; i++) memcpy(buf + i * s->len, s->data, s->len);
    buf[total] = '\0';
    LumenVal r = lumen_str_new(buf);
    xfree(buf);
    return r;
}

LumenVal lumen_str_title(LumenVal v) {
    LumenStr *s = as_str(v);
    LumenVal r = lumen_str_new(s->data);
    LumenStr *rs = unbox_ptr(r);
    int at_word_start = 1;
    for (size_t i = 0; i < rs->len; i++) {
        unsigned char b = (unsigned char)rs->data[i];
        int is_ws = lumen_is_ws_ch((char)b);
        if (at_word_start) {
            if (b >= 'a' && b <= 'z') rs->data[i] = (char)(b - 32);
        } else {
            if (b >= 'A' && b <= 'Z') rs->data[i] = (char)(b + 32);
        }
        at_word_start = is_ws;
    }
    return r;
}

LumenVal lumen_str_join(LumenVal sepv, LumenVal lst) {
    LumenStr *sep = as_str(sepv);
    LumenList *l = unbox_ptr(lst);
    size_t total = 0;
    for (size_t i = 0; i < l->len; i++) total += as_str(l->items[i])->len + sep->len;
    char *buf = xalloc(total + 1);
    buf[0] = 0;
    size_t pos = 0;
    for (size_t i = 0; i < l->len; i++) {
        if (i) { memcpy(buf + pos, sep->data, sep->len); pos += sep->len; }
        LumenStr *e = as_str(l->items[i]);
        memcpy(buf + pos, e->data, e->len); pos += e->len;
    }
    buf[pos] = 0;
    LumenVal r = lumen_str_new(buf);
    free(buf);
    return r;
}

LumenVal lumen_list_contains(LumenVal lst, LumenVal v) {
    LumenList *l = unbox_ptr(lst);
    for (size_t i = 0; i < l->len; i++) if (vals_eq(l->items[i], v)) return lumen_true();
    return lumen_false();
}

LumenVal lumen_list_index(LumenVal lst, LumenVal v) {
    LumenList *l = unbox_ptr(lst);
    for (size_t i = 0; i < l->len; i++) if (vals_eq(l->items[i], v)) return lumen_from_int((int64_t)i);
    return lumen_from_int(-1);
}

LumenVal lumen_list_count(LumenVal lst, LumenVal v) {
    LumenList *l = unbox_ptr(lst);
    int64_t n = 0;
    for (size_t i = 0; i < l->len; i++) if (vals_eq(l->items[i], v)) n++;
    return lumen_from_int(n);
}

static int obj_kind_of(LumenVal v) {
    if (lumen_is_ptr(v)) return ((LumenObj *)unbox_ptr(v))->kind;
    return -1;
}

LumenVal lumen_map_has(LumenVal mv, LumenVal key);
LumenVal lumen_contains(LumenVal recv, LumenVal needle) {
    if (obj_kind_of(recv) == OBJ_LIST) return lumen_list_contains(recv, needle);
    if (obj_kind_of(recv) == OBJ_MAP) return lumen_map_has(recv, needle);
    return lumen_str_contains(recv, needle);
}

LumenVal lumen_in(LumenVal x, LumenVal container) {
    return lumen_contains(container, x);
}
LumenVal lumen_not_in(LumenVal x, LumenVal container) {
    return lumen_bool(!lumen_truthy(lumen_contains(container, x)));
}

LumenVal lumen_join(LumenVal recv, LumenVal arg) {
    if (obj_kind_of(recv) == OBJ_STR && obj_kind_of(arg) == OBJ_LIST)
        return lumen_str_join(recv, arg);
    if (obj_kind_of(recv) == OBJ_LIST && obj_kind_of(arg) == OBJ_STR)
        return lumen_str_join(arg, recv);
    lumen_die("join expects a string and a list");
}

LumenVal lumen_map_new(void) {
    LumenMap *m = xalloc(sizeof(LumenMap));
    gc_register(m);
    m->kind = OBJ_MAP;
    m->rc = 1;
    m->len = 0;
    m->cap = 4;
    m->keys = xalloc(m->cap * sizeof(LumenVal));
    m->vals = xalloc(m->cap * sizeof(LumenVal));
    m->idx_cap = 8;
    m->idx = xalloc(m->idx_cap * sizeof(size_t));
    return box_ptr(m);
}

static LumenMap *as_map(LumenVal v) {
    if (obj_kind_of(v) == OBJ_MAP) return (LumenMap *)unbox_ptr(v);
    lumen_die("expected a map");
}

static void map_idx_put(LumenMap *m, size_t slot) {
    size_t mask = m->idx_cap - 1;
    size_t i = val_hash(m->keys[slot]) & mask;
    while (m->idx[i]) i = (i + 1) & mask;
    m->idx[i] = slot + 1;
}

static void map_idx_rebuild(LumenMap *m, size_t want) {
    while (m->idx_cap < want * 2) m->idx_cap *= 2;
    m->idx = realloc(m->idx, m->idx_cap * sizeof(size_t));
    memset(m->idx, 0, m->idx_cap * sizeof(size_t));
    for (size_t s = 0; s < m->len; s++) map_idx_put(m, s);
}

static ptrdiff_t map_find(LumenMap *m, LumenVal key) {
    if (m->len == 0) return -1;
    size_t mask = m->idx_cap - 1;
    size_t i = val_hash(key) & mask;
    while (m->idx[i]) {
        size_t slot = m->idx[i] - 1;
        if (vals_eq(m->keys[slot], key)) return (ptrdiff_t)slot;
        i = (i + 1) & mask;
    }
    return -1;
}

void lumen_map_set(LumenVal mv, LumenVal key, LumenVal val) {
    LumenMap *m = as_map(mv);
    ptrdiff_t found = map_find(m, key);
    if (found >= 0) { m->vals[found] = val; return; }
    if (m->len == m->cap) {
        m->cap *= 2;
        m->keys = realloc(m->keys, m->cap * sizeof(LumenVal));
        m->vals = realloc(m->vals, m->cap * sizeof(LumenVal));
    }
    size_t slot = m->len;
    m->keys[slot] = key;
    m->vals[slot] = val;
    m->len++;

    if ((m->len) * 10 >= m->idx_cap * 7) {
        map_idx_rebuild(m, m->len);
    } else {
        map_idx_put(m, slot);
    }
}

LumenVal lumen_map_get(LumenVal mv, LumenVal key) {
    LumenMap *m = as_map(mv);
    ptrdiff_t found = map_find(m, key);
    if (found >= 0) return m->vals[found];
    return lumen_nil();
}

LumenVal lumen_map_get_or(LumenVal mv, LumenVal key, LumenVal dflt) {
    LumenMap *m = as_map(mv);
    ptrdiff_t found = map_find(m, key);
    return found >= 0 ? m->vals[found] : dflt;
}

LumenVal lumen_map_has(LumenVal mv, LumenVal key) {
    LumenMap *m = as_map(mv);
    return map_find(m, key) >= 0 ? lumen_true() : lumen_false();
}

LumenVal lumen_map_keys(LumenVal mv) {
    LumenMap *m = as_map(mv);
    LumenVal out = lumen_list_new((int64_t)m->len);
    for (size_t i = 0; i < m->len; i++) lumen_list_push(out, m->keys[i]);
    return out;
}
LumenVal lumen_map_values(LumenVal mv) {
    LumenMap *m = as_map(mv);
    LumenVal out = lumen_list_new((int64_t)m->len);
    for (size_t i = 0; i < m->len; i++) lumen_list_push(out, m->vals[i]);
    return out;
}

LumenVal lumen_map_remove(LumenVal mv, LumenVal key) {
    LumenMap *m = as_map(mv);
    ptrdiff_t found = map_find(m, key);
    if (found < 0) return lumen_nil();
    size_t i = (size_t)found;
    LumenVal v = m->vals[i];
    for (size_t j = i; j + 1 < m->len; j++) {
        m->keys[j] = m->keys[j + 1];
        m->vals[j] = m->vals[j + 1];
    }
    m->len--;

    map_idx_rebuild(m, m->len);
    return v;
}

LumenVal lumen_index_get(LumenVal obj, LumenVal key) {
    int k = obj_kind_of(obj);
    if (k == OBJ_MAP) return lumen_map_get(obj, key);
    if (k == OBJ_LIST) return lumen_list_get(obj, lumen_to_int(key));
    if (k == OBJ_STR) {
        LumenStr *s = (LumenStr *)unbox_ptr(obj);
        int64_t i = lumen_to_int(key);
        if (i < 0 || (size_t)i >= s->len) { lumen_die("index out of range"); }
        char buf[2] = { s->data[i], 0 };
        return lumen_str_new(buf);
    }
    lumen_die("cannot index this value");
}
void lumen_index_set(LumenVal obj, LumenVal key, LumenVal val) {
    int k = obj_kind_of(obj);
    if (k == OBJ_MAP) { lumen_map_set(obj, key, val); return; }
    if (k == OBJ_LIST) { lumen_list_set(obj, lumen_to_int(key), val); return; }
    lumen_die("cannot index-assign this value");
}

LumenVal lumen_iter_prep(LumenVal obj) {
    if (obj_kind_of(obj) == OBJ_MAP) return lumen_map_keys(obj);
    if (obj_kind_of(obj) == OBJ_STR) {
        LumenStr *s = (LumenStr *)unbox_ptr(obj);
        LumenVal out = lumen_list_new((int64_t)s->len);
        char buf[2];
        buf[1] = '\0';
        for (size_t i = 0; i < s->len; i++) {
            buf[0] = s->data[i];
            lumen_list_push(out, lumen_str_new(buf));
        }
        return out;
    }
    if (obj_kind_of(obj) == OBJ_LIST) {

        LumenList *l = (LumenList *)unbox_ptr(obj);
        LumenVal out = lumen_list_new((int64_t)l->len);
        for (size_t i = 0; i < l->len; i++) lumen_list_push(out, l->items[i]);
        return out;
    }
    return obj;
}

static void slice_bounds(int64_t lo, int64_t hi, int64_t len, int64_t *pa, int64_t *pz) {
    int64_t a = lo < 0 ? lo + len : lo;
    int64_t z = hi < 0 ? hi + len : hi;
    if (a < 0) a = 0;
    if (a > len) a = len;
    if (z < 0) z = 0;
    if (z > len) z = len;
    if (a >= z) z = a;
    *pa = a;
    *pz = z;
}

LumenVal lumen_slice(LumenVal obj, LumenVal lov, LumenVal hiv) {
    int kind = obj_kind_of(obj);
    int64_t len = 0;
    if (kind == OBJ_LIST) len = (int64_t)((LumenList *)unbox_ptr(obj))->len;
    else if (kind == OBJ_STR) len = (int64_t)((LumenStr *)unbox_ptr(obj))->len;
    else { lumen_die("can only slice a list or string"); return lumen_nil(); }

    int64_t lo = lumen_is_nil(lov) ? 0 : lumen_to_int(lov);
    int64_t hi = lumen_is_nil(hiv) ? len : lumen_to_int(hiv);
    int64_t a, z;
    slice_bounds(lo, hi, len, &a, &z);

    if (kind == OBJ_LIST) {
        LumenList *l = (LumenList *)unbox_ptr(obj);
        LumenVal out = lumen_list_new(z - a);
        for (int64_t i = a; i < z; i++) lumen_list_push(out, l->items[i]);
        return out;
    }
    LumenStr *s = (LumenStr *)unbox_ptr(obj);
    int64_t n = z - a;
    char *buf = xalloc((size_t)n + 1);
    for (int64_t i = 0; i < n; i++) buf[i] = s->data[a + i];
    buf[n] = '\0';
    LumenVal out = lumen_str_new(buf);
    xfree(buf);
    return out;
}
void lumen_list_reverse(LumenVal lst) {
    LumenList *l = unbox_ptr(lst);
    for (size_t i = 0, j = l->len ? l->len - 1 : 0; i < j; i++, j--) {
        LumenVal t = l->items[i];
        l->items[i] = l->items[j];
        l->items[j] = t;
    }
}

void lumen_list_sort(LumenVal lst) {
    LumenList *l = unbox_ptr(lst);
    for (size_t i = 1; i < l->len; i++) {
        LumenVal key = l->items[i];
        size_t j = i;
        while (j > 0 && num_cmp(l->items[j - 1], key) > 0) {
            l->items[j] = l->items[j - 1];
            j--;
        }
        l->items[j] = key;
    }
}

LumenVal lumen_func_new(void *code, int64_t arity) {
    LumenFunc *fn = xalloc(sizeof(LumenFunc));
    gc_register(fn);
    fn->kind = OBJ_FUNC;
    fn->rc = 1;
    fn->code = code;
    fn->arity = (int)arity;
    fn->ncap = 0;
    fn->caps = NULL;
    return box_ptr(fn);
}

LumenVal lumen_closure_new(void *code, int64_t arity, int64_t ncap) {
    LumenFunc *fn = xalloc(sizeof(LumenFunc));
    gc_register(fn);
    fn->kind = OBJ_FUNC;
    fn->rc = 1;
    fn->code = code;
    fn->arity = (int)arity;
    fn->ncap = (int)ncap;
    fn->caps = ncap > 0 ? xalloc((size_t)ncap * sizeof(LumenVal)) : NULL;
    return box_ptr(fn);
}

void lumen_closure_set_cap(LumenVal clo, int64_t i, LumenVal v) {
    LumenFunc *fn = unbox_ptr(clo);
    fn->caps[i] = v;
}

typedef LumenVal (*Fn0)(void);
typedef LumenVal (*Fn1)(LumenVal);
typedef LumenVal (*Fn2)(LumenVal, LumenVal);
typedef LumenVal (*Fn3)(LumenVal, LumenVal, LumenVal);
typedef LumenVal (*Fn4)(LumenVal, LumenVal, LumenVal, LumenVal);
typedef LumenVal (*Fn5)(LumenVal, LumenVal, LumenVal, LumenVal, LumenVal);
typedef LumenVal (*Fn6)(LumenVal, LumenVal, LumenVal, LumenVal, LumenVal, LumenVal);
typedef LumenVal (*Fn7)(LumenVal, LumenVal, LumenVal, LumenVal, LumenVal, LumenVal, LumenVal);
typedef LumenVal (*Fn8)(LumenVal, LumenVal, LumenVal, LumenVal, LumenVal, LumenVal, LumenVal, LumenVal);

static LumenFunc *as_func(LumenVal f) {
    if (obj_kind_of(f) != OBJ_FUNC) { lumen_die("call of non-function value"); }
    return (LumenFunc *)unbox_ptr(f);
}

static LumenVal call_dispatch(void *code, int n, LumenVal *a) {
    switch (n) {
        case 0: return ((Fn0)code)();
        case 1: return ((Fn1)code)(a[0]);
        case 2: return ((Fn2)code)(a[0], a[1]);
        case 3: return ((Fn3)code)(a[0], a[1], a[2]);
        case 4: return ((Fn4)code)(a[0], a[1], a[2], a[3]);
        case 5: return ((Fn5)code)(a[0], a[1], a[2], a[3], a[4]);
        case 6: return ((Fn6)code)(a[0], a[1], a[2], a[3], a[4], a[5]);
        case 7: return ((Fn7)code)(a[0], a[1], a[2], a[3], a[4], a[5], a[6]);
        case 8: return ((Fn8)code)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]);
        default: lumen_die("closure call with too many total arguments (max 8)");
    }
    return lumen_nil();
}

static LumenVal call_closure(LumenVal f, LumenVal *args, int argc) {
    LumenFunc *fn = as_func(f);
    int total = fn->ncap + argc;
    LumenVal buf[8];
    if (total > 8) lumen_die("closure call with too many total arguments (max 8)");
    for (int i = 0; i < fn->ncap; i++) buf[i] = fn->caps[i];
    for (int i = 0; i < argc; i++) buf[fn->ncap + i] = args[i];
    return call_dispatch(fn->code, total, buf);
}

LumenVal lumen_call0(LumenVal f) { return call_closure(f, NULL, 0); }
LumenVal lumen_call1(LumenVal f, LumenVal a) { LumenVal v[1] = {a}; return call_closure(f, v, 1); }
LumenVal lumen_call2(LumenVal f, LumenVal a, LumenVal b) { LumenVal v[2] = {a, b}; return call_closure(f, v, 2); }
LumenVal lumen_call3(LumenVal f, LumenVal a, LumenVal b, LumenVal c) { LumenVal v[3] = {a, b, c}; return call_closure(f, v, 3); }
LumenVal lumen_call4(LumenVal f, LumenVal a, LumenVal b, LumenVal c, LumenVal d) {
    LumenVal v[4] = {a, b, c, d};
    return call_closure(f, v, 4);
}

#define LUMEN_CB_POOL 16
static LumenVal lumen_cb_fns[LUMEN_CB_POOL];
static int      lumen_cb_used[LUMEN_CB_POOL];

static int64_t lumen_cb_dispatch(int idx, int64_t a0, int64_t a1, int64_t a2, int64_t a3) {
    if (idx < 0 || idx >= LUMEN_CB_POOL || !lumen_cb_used[idx]) return 0;
    LumenVal f = lumen_cb_fns[idx];
    LumenFunc *fn = as_func(f);
    int n = fn->arity - fn->ncap;
    if (n < 0) n = 0;
    if (n > 4) n = 4;
    LumenVal av[4] = {
        lumen_from_int(a0), lumen_from_int(a1),
        lumen_from_int(a2), lumen_from_int(a3),
    };

    gc_pause_depth++;
    LumenVal r = call_closure(f, av, n);
    gc_pause_depth--;
    return lumen_ffi_argint(r);
}

#define LUMEN_CB_THUNK(N) \
    static int64_t lumen_cb_thunk_##N(int64_t a0, int64_t a1, int64_t a2, int64_t a3) { \
        return lumen_cb_dispatch(N, a0, a1, a2, a3); \
    }
LUMEN_CB_THUNK(0)  LUMEN_CB_THUNK(1)  LUMEN_CB_THUNK(2)  LUMEN_CB_THUNK(3)
LUMEN_CB_THUNK(4)  LUMEN_CB_THUNK(5)  LUMEN_CB_THUNK(6)  LUMEN_CB_THUNK(7)
LUMEN_CB_THUNK(8)  LUMEN_CB_THUNK(9)  LUMEN_CB_THUNK(10) LUMEN_CB_THUNK(11)
LUMEN_CB_THUNK(12) LUMEN_CB_THUNK(13) LUMEN_CB_THUNK(14) LUMEN_CB_THUNK(15)

static void *const lumen_cb_thunks[LUMEN_CB_POOL] = {
    (void *)lumen_cb_thunk_0,  (void *)lumen_cb_thunk_1,  (void *)lumen_cb_thunk_2,  (void *)lumen_cb_thunk_3,
    (void *)lumen_cb_thunk_4,  (void *)lumen_cb_thunk_5,  (void *)lumen_cb_thunk_6,  (void *)lumen_cb_thunk_7,
    (void *)lumen_cb_thunk_8,  (void *)lumen_cb_thunk_9,  (void *)lumen_cb_thunk_10, (void *)lumen_cb_thunk_11,
    (void *)lumen_cb_thunk_12, (void *)lumen_cb_thunk_13, (void *)lumen_cb_thunk_14, (void *)lumen_cb_thunk_15,
};

LumenVal lumen_cb_register(LumenVal fn) {
    if (obj_kind_of(fn) != OBJ_FUNC) lumen_die("callback: argument must be a function");
    for (int i = 0; i < LUMEN_CB_POOL; i++) {
        if (!lumen_cb_used[i]) {
            lumen_cb_used[i] = 1;
            lumen_cb_fns[i] = fn;
            return lumen_from_int((int64_t)(intptr_t)lumen_cb_thunks[i]);
        }
    }
    lumen_die("callback: too many live callbacks (max 16)");
}

LumenVal lumen_ord(LumenVal v) {
    LumenStr *s = as_str(v);
    return lumen_from_int(s->len ? (unsigned char)s->data[0] : 0);
}

LumenVal lumen_chr(LumenVal v) {
    char buf[2] = { (char)(unsigned char)lumen_to_int(v), 0 };
    return lumen_str_new(buf);
}
LumenVal lumen_is_digit(LumenVal v) {
    LumenStr *s = as_str(v);
    int c = s->len ? (unsigned char)s->data[0] : 0;
    return lumen_bool(c >= '0' && c <= '9');
}
LumenVal lumen_is_alpha(LumenVal v) {
    LumenStr *s = as_str(v);
    int c = s->len ? (unsigned char)s->data[0] : 0;
    return lumen_bool((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z'));
}
LumenVal lumen_is_space(LumenVal v) {
    LumenStr *s = as_str(v);
    int c = s->len ? (unsigned char)s->data[0] : 0;
    return lumen_bool(c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v');
}

LumenVal lumen_input(LumenVal prompt) {
    if (lumen_is_ptr(prompt) && obj_kind_of(prompt) == OBJ_STR) {
        fputs(((LumenStr *)unbox_ptr(prompt))->data, stdout);
        fflush(stdout);
    }
    size_t cap = 128, len = 0;
    char *buf = xalloc(cap);
    int ch;
    while ((ch = fgetc(stdin)) != EOF && ch != '\n') {
        if (ch == '\r') continue;
        if (len + 1 >= cap) { cap *= 2; buf = realloc(buf, cap); }
        buf[len++] = (char)ch;
    }
    if (ch == EOF && len == 0) return lumen_nil();
    buf[len] = 0;
    LumenVal r = lumen_str_new(buf);
    free(buf);
    return r;
}
