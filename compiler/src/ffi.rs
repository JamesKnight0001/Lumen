//! Windows-only FFI for the interpreter. Loads DLLs at runtime and calls into
//! them through two hand-written asm trampolines. The tricky part is the Win64
//! calling convention: a float arg can arrive in either a GP register or an XMM
//! register depending on the callee's signature, which we can't know here, so we
//! load every argument into BOTH and let the callee read whichever it expects.
//! The COM trampoline additionally threads `this` and indexes a vtable.
#![cfg(windows)]

use crate::ast::{ExternFn, Type};
use crate::interp::Value;
use std::arch::global_asm;
use std::ffi::CString;
use std::os::raw::{c_char, c_void};

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(name: *const c_char) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
}

global_asm!(
    r#"
    .globl lumen_ffi_trampoline
lumen_ffi_trampoline:
    // rcx = target, rdx = *args[4], r8 = argc, r9 = *out_xmm (for float ret)
    push rbp
    mov rbp, rsp
    sub rsp, 48            // 32 shadow + 8 to save r9 + 8 pad (keeps 16-align)
    mov [rbp-8], r9        // save out_xmm ptr
    mov rax, rcx           // rax = target fn
    // load up to 4 args into both GP and XMM (read from the args array in rdx)
    mov rcx, [rdx]
    movq xmm0, [rdx]
    mov r10, [rdx+8]
    movq xmm1, [rdx+8]
    mov r8,  [rdx+16]
    movq xmm2, [rdx+16]
    mov r9,  [rdx+24]
    movq xmm3, [rdx+24]
    mov rdx, r10           // arg1 -> rdx (do this last; r10 held it)
    call rax
    // store the float result for the caller
    mov r10, [rbp-8]
    movq [r10], xmm0
    mov rsp, rbp
    pop rbp
    ret
"#
);

global_asm!(
    r#"
    .globl lumen_com_trampoline
lumen_com_trampoline:
    // rcx=fnptr rdx=obj r8=nargs r9=args; out_xmm = 5th arg (stack)
    push rbp
    mov rbp, rsp
    push rsi
    push rdi
    push rbx
    mov rax, rcx           // fnptr
    mov rsi, rdx           // obj (this)
    mov rbx, r8            // nargs
    mov rdi, r9            // args ptr
    mov r10, [rbp+48]      // out_xmm (ret8+rbp8+3 pushes24 = 40; +8 = first stack arg at 48)
    push r10               // save out_xmm at [rbp-32]
    // reserve round16(32 + 8*max(0,nargs-3))
    mov r11, rbx
    sub r11, 3
    jg 1f
    xor r11, r11
1:  lea rcx, [r11*8+32]
    add rcx, 15
    and rcx, -16
    sub rsp, rcx
    and rsp, -16           // 16-align the call frame (5 pushes left rsp at +8)
    // copy stack args (3..nargs-1) to [rsp+32+8*(i-3)]
    mov r10, 3
2:  cmp r10, rbx
    jge 3f
    mov r11, [rdi+r10*8]
    lea rcx, [r10-3]
    mov [rsp+rcx*8+32], r11
    inc r10
    jmp 2b
3:  mov rcx, rsi           // this -> rcx
    cmp rbx, 1
    jl 9f
    mov rdx, [rdi]
    movq xmm1, [rdi]
    cmp rbx, 2
    jl 9f
    mov r8, [rdi+8]
    movq xmm2, [rdi+8]
    cmp rbx, 3
    jl 9f
    mov r9, [rdi+16]
    movq xmm3, [rdi+16]
9:  call rax
    lea rsp, [rbp-32]      // drop stack-arg area, back to out_xmm slot
    pop r10                // out_xmm
    movq [r10], xmm0
    pop rbx
    pop rdi
    pop rsi
    pop rbp
    ret
"#
);

extern "system" {
    fn lumen_com_trampoline(
        fnptr: *const core::ffi::c_void,
        obj: *const core::ffi::c_void,
        nargs: i64,
        args: *const i64,
        out_xmm: *mut i64,
    ) -> i64;
}

// Safe-ish wrapper over the COM trampoline. Used for vtable calls where the
// first argument is the object pointer (`this`) and fnptr is resolved from the
// object's vtable by the caller (see builtins::com_vcall).
#[allow(clippy::missing_safety_doc)]
pub unsafe fn com_trampoline(
    fnptr: *const core::ffi::c_void,
    obj: *const core::ffi::c_void,
    nargs: i64,
    args: *const i64,
    out_xmm: *mut i64,
) -> i64 {
    lumen_com_trampoline(fnptr, obj, nargs, args, out_xmm)
}

extern "system" {
    fn lumen_ffi_trampoline(
        target: *mut c_void,
        args: *const i64,
        argc: i64,
        out_xmm: *mut i64,
    ) -> i64;
}

// Marshal one Lumen Value into a raw 64-bit word. Floats are passed as their
// IEEE bit pattern (the trampoline copies it into XMM too). Strings are pinned
// as CStrings in `pin` so their pointers stay valid until after the call.
fn val_word(v: &Value, want_float: bool, pin: &mut Vec<CString>) -> Result<i64, String> {
    if want_float {
        let d = match v {
            Value::Float(x) => *x,
            Value::Int(n) => *n as f64,
            Value::Bool(b) => (*b as i64) as f64,
            _ => return Err("FFI: float parameter needs a number".into()),
        };
        return Ok(d.to_bits() as i64);
    }
    Ok(match v {
        Value::Int(n) => *n,
        Value::Bool(b) => *b as i64,
        Value::Nil => 0,
        Value::Str(s) => {
            let c = CString::new(s.as_str()).map_err(|_| "string has interior NUL")?;
            let ptr = c.as_ptr() as i64;
            pin.push(c);
            ptr
        }
        Value::Float(x) => *x as i64,

        Value::CBuf(b) => b.borrow().as_ptr() as i64,
        _ => return Err("unsupported FFI argument type".into()),
    })
}

fn is_fty(t: &Type) -> bool {
    matches!(t, Type::Named(n) if n == "f64" || n == "f32")
}

pub fn call_dll(spec: &(String, ExternFn), args: &[Value]) -> Result<Value, String> {
    let (lib, ef) = spec;
    if args.len() > 4 {

        return Err(format!(
            "FFI: the interpreter supports at most 4 arguments (got {}); \
             compile with `lumen build` for more",
            args.len()
        ));
    }
    unsafe {
        let lib_c = CString::new(lib.as_str()).unwrap();
        let module = LoadLibraryA(lib_c.as_ptr());
        if module.is_null() {
            return Err(format!("FFI: could not load library '{lib}'"));
        }
        let fn_c = CString::new(ef.name.as_str()).unwrap();
        let proc = GetProcAddress(module, fn_c.as_ptr());
        if proc.is_null() {
            return Err(format!("FFI: symbol '{}' not found in '{}'", ef.name, lib));
        }

        let mut pin: Vec<CString> = Vec::new();
        let mut words = [0i64; 4];
        for (i, v) in args.iter().enumerate() {
            let want_float = ef
                .params
                .get(i)
                .map(|p| is_fty(&p.ty))
                .unwrap_or(false);
            words[i] = val_word(v, want_float, &mut pin)?;
        }

        let mut out_xmm: i64 = 0;
        // The trampoline returns the integer result in rax and copies xmm0 into
        // out_xmm, so we can pick the right one based on the declared return type.
        let rax = lumen_ffi_trampoline(proc, words.as_ptr(), args.len() as i64, &mut out_xmm);
        drop(pin);

        Ok(match &ef.ret {
            Type::Nil => Value::Nil,
            Type::Named(n) if n == "f64" || n == "f32" => {
                Value::Float(f64::from_bits(out_xmm as u64))
            }
            _ => Value::Int(rax),
        })
    }
}
