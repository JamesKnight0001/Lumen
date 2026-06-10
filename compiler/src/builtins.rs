
//! Built-in standard-library modules (math, os, rand, time, json, cffi, ...).
//! The MODULE_FUNCS table near the bottom is the single source of truth: every
//! entry binds a module.name to its arity, the runtime C symbol, and an `eval`
//! closure the interpreter uses. is_module/lookup both read this one table, so
//! adding a builtin is a single edit here.
use crate::interp::Value;

pub struct ModuleFn {

    pub module: &'static str,

    pub name: &'static str,

    pub arity: u8,

    pub symbol: &'static str,

    pub eval: fn(&[Value]) -> Result<Value, String>,
}

fn as_f(v: &Value) -> Result<f64, String> {
    match v {
        Value::Float(x) => Ok(*x),
        Value::Int(n) => Ok(*n as f64),
        _ => Err("math: expected a number".into()),
    }
}

fn arg(args: &[Value], i: usize) -> Result<&Value, String> {
    args.get(i)
        .ok_or_else(|| "math: missing argument".to_string())
}

fn as_str(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        _ => String::new(),
    }
}

fn str_arg(args: &[Value], i: usize) -> String {
    args.get(i).map(as_str).unwrap_or_default()
}

fn str_val(s: String) -> Value {
    Value::Str(std::rc::Rc::new(s))
}

fn list_val(items: Vec<Value>) -> Value {
    Value::List(std::rc::Rc::new(std::cell::RefCell::new(items)))
}

fn cbuf_of(v: &Value) -> Result<std::rc::Rc<std::cell::RefCell<Vec<u8>>>, String> {
    match v {
        Value::CBuf(b) => Ok(b.clone()),
        _ => Err("cffi: expected a cbuf".into()),
    }
}

fn int_of(v: &Value) -> i64 {
    match v {
        Value::Int(n) => *n,
        Value::Bool(b) => *b as i64,
        Value::Float(x) => *x as i64,
        _ => 0,
    }
}

fn float_of(v: &Value) -> f64 {
    match v {
        Value::Float(x) => *x,
        Value::Int(n) => *n as f64,
        Value::Bool(b) => (*b as i64) as f64,
        _ => 0.0,
    }
}

fn cbuf_bounds(len: usize, off: i64, width: usize, who: &str) -> Result<usize, String> {
    if off < 0 || off as usize + width > len {
        return Err(format!("{who}: offset {off} out of bounds (size {len})"));
    }
    Ok(off as usize)
}

fn cset(
    args: &[Value],
    width: usize,
    who: &str,
    write: impl Fn(&mut [u8]),
) -> Result<Value, String> {
    let b = cbuf_of(arg(args, 0)?)?;
    let off = int_of(arg(args, 1)?);
    let mut data = b.borrow_mut();
    let o = cbuf_bounds(data.len(), off, width, who)?;
    write(&mut data[o..o + width]);
    Ok(Value::Nil)
}

fn cget_bytes(args: &[Value], width: usize, who: &str) -> Result<(Vec<u8>, ()), String> {
    let b = cbuf_of(arg(args, 0)?)?;
    let off = int_of(arg(args, 1)?);
    let data = b.borrow();
    let o = cbuf_bounds(data.len(), off, width, who)?;
    Ok((data[o..o + width].to_vec(), ()))
}

#[cfg(windows)]
fn com_vcall_impl(obj: i64, slot: i64, args: &[u8], ret_kind: i64) -> Result<Value, String> {
    if obj == 0 {
        return Err("vcall: null object".into());
    }
    if slot < 0 {
        return Err("vcall: negative slot".into());
    }
    let nargs = (args.len() / 8) as i64;

    let mut words = [0i64; 8];
    for i in 0..(nargs as usize).min(8) {
        let mut w = [0u8; 8];
        w.copy_from_slice(&args[i * 8..i * 8 + 8]);
        words[i] = i64::from_le_bytes(w);
    }
    unsafe {
        // COM ABI: a COM object's first word points at its vtable; the vtable is
        // an array of function pointers. Index `slot` to get the method, then call
        // it with the object as the implicit first arg (`this`).
        let obj_p = obj as usize as *const *const usize;
        let vtbl = *obj_p;
        let fnptr = *vtbl.add(slot as usize) as *const core::ffi::c_void;
        let mut out_xmm: i64 = 0;
        let rax = crate::ffi::com_trampoline(
            fnptr,
            obj as usize as *const core::ffi::c_void,
            nargs,
            words.as_ptr(),
            &mut out_xmm,
        );
        Ok(if ret_kind == 1 {
            Value::Float(f64::from_bits(out_xmm as u64))
        } else {
            Value::Int(rax)
        })
    }
}

#[cfg(not(windows))]
fn com_vcall_impl(_obj: i64, _slot: i64, _args: &[u8], _ret_kind: i64) -> Result<Value, String> {
    Err("vcall: COM/vtable calls are only supported on Windows".into())
}

// Format a Unix epoch as UTC "YYYY-MM-DD HH:MM:SS" using Howard Hinnant's
// days-from-civil algorithm (the 719468 / 146097 era math), so we avoid pulling
// in a date library. div_euclid/rem_euclid keep it correct for negative epochs.
fn fmt_epoch_utc(epoch_secs: i64) -> String {

    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let hour = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let sec = secs_of_day % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02} {hour:02}:{min:02}:{sec:02}")
}

static RNG_STATE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0x9E37_79B9_7F4A_7C15);

fn rng_set(seed: u64) {
    RNG_STATE.store(seed, std::sync::atomic::Ordering::Relaxed);
}

// SplitMix64: advance the state by the golden-ratio constant, then run the
// finalizer mix. Cheap, seedable, and identical across runs so rand.seed gives
// reproducible sequences. Not cryptographically secure.
fn rng_next() -> u64 {
    use std::sync::atomic::Ordering;
    let next = RNG_STATE
        .load(Ordering::Relaxed)
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    RNG_STATE.store(next, Ordering::Relaxed);
    let mut z = next;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn json_num(v: &Value) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(x) => {
            if x.fract() == 0.0 && x.is_finite() {
                format!("{x:.1}")
            } else {
                format!("{x}")
            }
        }
        _ => "null".to_string(),
    }
}

fn json_escape(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\u{0d}' => out.push_str("\\r"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_write(v: &Value, out: &mut String) {
    match v {
        Value::Int(_) | Value::Float(_) => out.push_str(&json_num(v)),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Nil => out.push_str("null"),
        Value::Str(s) => json_escape(s, out),
        Value::List(items) => {
            out.push('[');
            for (i, el) in items.borrow().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_write(el, out);
            }
            out.push(']');
        }
        Value::Map(entries) => {
            out.push('{');
            for (i, (k, val)) in entries.borrow().pairs().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }

                json_escape(&json_key_str(k), out);
                out.push(':');
                json_write(val, out);
            }
            out.push('}');
        }

        _ => out.push_str("null"),
    }
}

fn json_key_str(k: &Value) -> String {
    match k {
        Value::Str(s) => s.to_string(),
        Value::Int(_) | Value::Float(_) => json_num(k),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "nil".to_string(),
        _ => String::new(),
    }
}

fn json_skip_ws(b: &[u8], pos: &mut usize) {
    while *pos < b.len() && matches!(b[*pos], b' ' | b'\t' | b'\n' | 0x0d) {
        *pos += 1;
    }
}

fn json_parse_value(b: &[u8], pos: &mut usize) -> Option<Value> {
    json_skip_ws(b, pos);
    if *pos >= b.len() {
        return None;
    }
    match b[*pos] {
        b'"' => json_parse_string(b, pos).map(str_val),
        b'{' => parse_obj(b, pos),
        b'[' => parse_arr(b, pos),
        b'-' | b'0'..=b'9' => parse_num(b, pos),
        b't' if b[*pos..].starts_with(b"true") => {
            *pos += 4;
            Some(Value::Bool(true))
        }
        b'f' if b[*pos..].starts_with(b"false") => {
            *pos += 5;
            Some(Value::Bool(false))
        }
        b'n' if b[*pos..].starts_with(b"null") => {
            *pos += 4;
            Some(Value::Nil)
        }
        _ => None,
    }
}

fn json_parse_string(b: &[u8], pos: &mut usize) -> Option<String> {

    *pos += 1;
    let mut s = String::new();
    while *pos < b.len() && b[*pos] != b'"' {
        let c = b[*pos];
        *pos += 1;
        if c == b'\\' {
            if *pos >= b.len() {
                return None;
            }
            let e = b[*pos];
            *pos += 1;
            match e {
                b'"' => s.push('"'),
                b'\\' => s.push('\\'),
                b'/' => s.push('/'),
                b'n' => s.push('\n'),
                b't' => s.push('\t'),
                b'r' => s.push('\u{0d}'),
                b'b' => s.push('\u{08}'),
                b'f' => s.push('\u{0c}'),
                b'u' => {
                    if *pos + 4 > b.len() {
                        return None;
                    }
                    let hex = std::str::from_utf8(&b[*pos..*pos + 4]).ok()?;
                    let cp = u32::from_str_radix(hex, 16).ok()?;
                    *pos += 4;
                    s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                }
                other => s.push(other as char),
            }
        } else {

            s.push(c as char);
        }
    }
    if *pos >= b.len() || b[*pos] != b'"' {
        return None;
    }
    *pos += 1;
    Some(s)
}

fn parse_num(b: &[u8], pos: &mut usize) -> Option<Value> {
    let start = *pos;
    let mut is_float = false;
    if *pos < b.len() && b[*pos] == b'-' {
        *pos += 1;
    }
    while *pos < b.len() && b[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos < b.len() && b[*pos] == b'.' {
        is_float = true;
        *pos += 1;
        while *pos < b.len() && b[*pos].is_ascii_digit() {
            *pos += 1;
        }
    }
    if *pos < b.len() && (b[*pos] == b'e' || b[*pos] == b'E') {
        is_float = true;
        *pos += 1;
        if *pos < b.len() && (b[*pos] == b'+' || b[*pos] == b'-') {
            *pos += 1;
        }
        while *pos < b.len() && b[*pos].is_ascii_digit() {
            *pos += 1;
        }
    }
    let tok = std::str::from_utf8(&b[start..*pos]).ok()?;
    if is_float {
        tok.parse::<f64>().ok().map(Value::Float)
    } else {
        tok.parse::<i64>().ok().map(Value::Int)
    }
}

fn parse_arr(b: &[u8], pos: &mut usize) -> Option<Value> {
    *pos += 1;
    let items: std::rc::Rc<std::cell::RefCell<Vec<Value>>> = Default::default();
    json_skip_ws(b, pos);
    if *pos < b.len() && b[*pos] == b']' {
        *pos += 1;
        return Some(Value::List(items));
    }
    loop {
        let el = json_parse_value(b, pos)?;
        items.borrow_mut().push(el);
        json_skip_ws(b, pos);
        if *pos >= b.len() {
            return None;
        }
        match b[*pos] {
            b',' => {
                *pos += 1;
            }
            b']' => {
                *pos += 1;
                break;
            }
            _ => return None,
        }
    }
    Some(Value::List(items))
}

fn parse_obj(b: &[u8], pos: &mut usize) -> Option<Value> {
    *pos += 1;
    let mut pairs: Vec<(Value, Value)> = Vec::new();
    json_skip_ws(b, pos);
    if *pos < b.len() && b[*pos] == b'}' {
        *pos += 1;
        return Some(Value::Map(crate::interp::lumen_map(pairs)));
    }
    loop {
        json_skip_ws(b, pos);
        if *pos >= b.len() || b[*pos] != b'"' {
            return None;
        }
        let key = json_parse_string(b, pos)?;
        json_skip_ws(b, pos);
        if *pos >= b.len() || b[*pos] != b':' {
            return None;
        }
        *pos += 1;
        let val = json_parse_value(b, pos)?;
        // lumen_map dedups by key (last wins), matching object semantics.
        pairs.push((Value::Str(std::rc::Rc::new(key)), val));
        json_skip_ws(b, pos);
        if *pos >= b.len() {
            return None;
        }
        match b[*pos] {
            b',' => {
                *pos += 1;
            }
            b'}' => {
                *pos += 1;
                break;
            }
            _ => return None,
        }
    }
    Some(Value::Map(crate::interp::lumen_map(pairs)))
}

// The registry. Order doesn't matter; lookup is a linear scan. Each entry's
// `symbol` is the runtime function the native backend links against, while
// `eval` is what the interpreter runs, so the two backends stay in sync.
pub static MODULE_FUNCS: &[ModuleFn] = &[
    ModuleFn {
        module: "math",
        name: "sqrt",
        arity: 1,
        symbol: "lumen_math_sqrt",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.sqrt())),
    },
    ModuleFn {
        module: "math",
        name: "sin",
        arity: 1,
        symbol: "lumen_math_sin",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.sin())),
    },
    ModuleFn {
        module: "math",
        name: "cos",
        arity: 1,
        symbol: "lumen_math_cos",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.cos())),
    },
    ModuleFn {
        module: "math",
        name: "tan",
        arity: 1,
        symbol: "lumen_math_tan",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.tan())),
    },
    ModuleFn {
        module: "math",
        name: "abs",
        arity: 1,
        symbol: "lumen_math_abs",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.abs())),
    },
    ModuleFn {
        module: "math",
        name: "floor",
        arity: 1,
        symbol: "lumen_math_floor",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.floor())),
    },
    ModuleFn {
        module: "math",
        name: "ceil",
        arity: 1,
        symbol: "lumen_math_ceil",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.ceil())),
    },
    ModuleFn {
        module: "math",
        name: "pow",
        arity: 2,
        symbol: "lumen_math_pow",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.powf(as_f(arg(a, 1)?)?))),
    },
    ModuleFn {
        module: "math",
        name: "log",
        arity: 1,
        symbol: "lumen_math_log",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.ln())),
    },
    ModuleFn {
        module: "math",
        name: "log10",
        arity: 1,
        symbol: "lumen_math_log10",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.log10())),
    },
    ModuleFn {
        module: "math",
        name: "exp",
        arity: 1,
        symbol: "lumen_math_exp",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.exp())),
    },
    ModuleFn {
        module: "math",
        name: "pi",
        arity: 0,
        symbol: "lumen_math_pi",
        eval: |_| Ok(Value::Float(std::f64::consts::PI)),
    },
    ModuleFn {
        module: "math",
        name: "e",
        arity: 0,
        symbol: "lumen_math_e",
        eval: |_| Ok(Value::Float(std::f64::consts::E)),
    },
    ModuleFn {
        module: "math",
        name: "tau",
        arity: 0,
        symbol: "lumen_math_tau",
        eval: |_| Ok(Value::Float(std::f64::consts::TAU)),
    },
    ModuleFn {
        module: "math",
        name: "inf",
        arity: 0,
        symbol: "lumen_math_inf",
        eval: |_| Ok(Value::Float(f64::INFINITY)),
    },
    ModuleFn {
        module: "math",
        name: "log2",
        arity: 1,
        symbol: "lumen_math_log2",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.log2())),
    },
    ModuleFn {
        module: "math",
        name: "cbrt",
        arity: 1,
        symbol: "lumen_math_cbrt",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.cbrt())),
    },
    ModuleFn {
        module: "math",
        name: "asin",
        arity: 1,
        symbol: "lumen_math_asin",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.asin())),
    },
    ModuleFn {
        module: "math",
        name: "acos",
        arity: 1,
        symbol: "lumen_math_acos",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.acos())),
    },
    ModuleFn {
        module: "math",
        name: "atan",
        arity: 1,
        symbol: "lumen_math_atan",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.atan())),
    },
    ModuleFn {
        module: "math",
        name: "atan2",
        arity: 2,
        symbol: "lumen_math_atan2",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.atan2(as_f(arg(a, 1)?)?))),
    },
    ModuleFn {
        module: "math",
        name: "sinh",
        arity: 1,
        symbol: "lumen_math_sinh",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.sinh())),
    },
    ModuleFn {
        module: "math",
        name: "cosh",
        arity: 1,
        symbol: "lumen_math_cosh",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.cosh())),
    },
    ModuleFn {
        module: "math",
        name: "tanh",
        arity: 1,
        symbol: "lumen_math_tanh",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.tanh())),
    },
    ModuleFn {
        module: "math",
        name: "hypot",
        arity: 2,
        symbol: "lumen_math_hypot",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.hypot(as_f(arg(a, 1)?)?))),
    },
    ModuleFn {
        module: "math",
        name: "round",
        arity: 1,
        symbol: "lumen_math_round",

        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.round())),
    },
    ModuleFn {
        module: "math",
        name: "trunc",
        arity: 1,
        symbol: "lumen_math_trunc",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.trunc())),
    },
    ModuleFn {
        module: "math",
        name: "min",
        arity: 2,
        symbol: "lumen_math_min",

        eval: |a| {
            let x = as_f(arg(a, 0)?)?;
            let y = as_f(arg(a, 1)?)?;
            Ok(Value::Float(if x < y { x } else { y }))
        },
    },
    ModuleFn {
        module: "math",
        name: "max",
        arity: 2,
        symbol: "lumen_math_max",
        eval: |a| {
            let x = as_f(arg(a, 0)?)?;
            let y = as_f(arg(a, 1)?)?;
            Ok(Value::Float(if x > y { x } else { y }))
        },
    },
    ModuleFn {
        module: "math",
        name: "sign",
        arity: 1,
        symbol: "lumen_math_sign",
        eval: |a| {
            let x = as_f(arg(a, 0)?)?;
            Ok(Value::Float(if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }))
        },
    },
    ModuleFn {
        module: "math",
        name: "deg",
        arity: 1,
        symbol: "lumen_math_deg",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.to_degrees())),
    },
    ModuleFn {
        module: "math",
        name: "rad",
        arity: 1,
        symbol: "lumen_math_rad",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.to_radians())),
    },
    ModuleFn {
        module: "math",
        name: "isnan",
        arity: 1,
        symbol: "lumen_math_isnan",
        eval: |a| Ok(Value::Bool(as_f(arg(a, 0)?)?.is_nan())),
    },
    ModuleFn {
        module: "math",
        name: "isinf",
        arity: 1,
        symbol: "lumen_math_isinf",
        eval: |a| Ok(Value::Bool(as_f(arg(a, 0)?)?.is_infinite())),
    },

    ModuleFn {
        module: "math",
        name: "gcd",
        arity: 2,
        symbol: "lumen_math_gcd",
        eval: |a| {
            let mut x = (as_f(arg(a, 0)?)? as i64).abs();
            let mut y = (as_f(arg(a, 1)?)? as i64).abs();
            while y != 0 {
                let t = x % y;
                x = y;
                y = t;
            }
            Ok(Value::Int(x))
        },
    },
    ModuleFn {
        module: "math",
        name: "lcm",
        arity: 2,
        symbol: "lumen_math_lcm",
        eval: |a| {
            let x = (as_f(arg(a, 0)?)? as i64).abs();
            let y = (as_f(arg(a, 1)?)? as i64).abs();
            if x == 0 || y == 0 {
                return Ok(Value::Int(0));
            }
            let (mut g, mut h) = (x, y);
            while h != 0 {
                let t = g % h;
                g = h;
                h = t;
            }
            Ok(Value::Int((x / g).wrapping_mul(y)))
        },
    },
    ModuleFn {
        module: "math",
        name: "factorial",
        arity: 1,
        symbol: "lumen_math_factorial",
        eval: |a| {
            let k = as_f(arg(a, 0)?)? as i64;
            if k < 0 {
                return Ok(Value::Int(0));
            }
            let mut r: i64 = 1;
            for i in 2..=k {
                r = r.wrapping_mul(i);
            }
            Ok(Value::Int(r))
        },
    },

    ModuleFn {
        module: "math",
        name: "fmod",
        arity: 2,
        symbol: "lumen_math_fmod",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)? % as_f(arg(a, 1)?)?)),
    },
    ModuleFn {
        module: "math",
        name: "copysign",
        arity: 2,
        symbol: "lumen_math_copysign",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.copysign(as_f(arg(a, 1)?)?))),
    },
    ModuleFn {
        module: "math",
        name: "log1p",
        arity: 1,
        symbol: "lumen_math_log1p",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.ln_1p())),
    },
    ModuleFn {
        module: "math",
        name: "expm1",
        arity: 1,
        symbol: "lumen_math_expm1",
        eval: |a| Ok(Value::Float(as_f(arg(a, 0)?)?.exp_m1())),
    },
    ModuleFn {
        module: "math",
        name: "isfinite",
        arity: 1,
        symbol: "lumen_math_isfinite",
        eval: |a| Ok(Value::Bool(as_f(arg(a, 0)?)?.is_finite())),
    },

    ModuleFn {
        module: "os",
        name: "read",
        arity: 1,
        symbol: "lumen_os_read",
        eval: |a| {
            Ok(match std::fs::read(str_arg(a, 0)) {

                Ok(bytes) => str_val(String::from_utf8_lossy(&bytes).into_owned()),
                Err(_) => Value::Nil,
            })
        },
    },
    ModuleFn {
        module: "os",
        name: "write",
        arity: 2,
        symbol: "lumen_os_write",
        eval: |a| {
            Ok(Value::Bool(
                std::fs::write(str_arg(a, 0), str_arg(a, 1)).is_ok(),
            ))
        },
    },
    ModuleFn {
        module: "os",
        name: "append",
        arity: 2,
        symbol: "lumen_os_append",
        eval: |a| {
            use std::io::Write;
            let ok = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(str_arg(a, 0))
                .and_then(|mut f| f.write_all(str_arg(a, 1).as_bytes()))
                .is_ok();
            Ok(Value::Bool(ok))
        },
    },
    ModuleFn {
        module: "os",
        name: "exists",
        arity: 1,
        symbol: "lumen_os_exists",
        eval: |a| Ok(Value::Bool(std::path::Path::new(&str_arg(a, 0)).exists())),
    },
    ModuleFn {
        module: "os",
        name: "is_file",
        arity: 1,
        symbol: "lumen_os_is_file",
        eval: |a| Ok(Value::Bool(std::path::Path::new(&str_arg(a, 0)).is_file())),
    },
    ModuleFn {
        module: "os",
        name: "is_dir",
        arity: 1,
        symbol: "lumen_os_is_dir",
        eval: |a| Ok(Value::Bool(std::path::Path::new(&str_arg(a, 0)).is_dir())),
    },
    ModuleFn {
        module: "os",
        name: "remove",
        arity: 1,
        symbol: "lumen_os_remove",
        eval: |a| Ok(Value::Bool(std::fs::remove_file(str_arg(a, 0)).is_ok())),
    },
    ModuleFn {
        module: "os",
        name: "rmdir",
        arity: 1,
        symbol: "lumen_os_rmdir",
        eval: |a| Ok(Value::Bool(std::fs::remove_dir(str_arg(a, 0)).is_ok())),
    },
    ModuleFn {
        module: "os",
        name: "rename",
        arity: 2,
        symbol: "lumen_os_rename",
        eval: |a| {
            Ok(Value::Bool(
                std::fs::rename(str_arg(a, 0), str_arg(a, 1)).is_ok(),
            ))
        },
    },
    ModuleFn {
        module: "os",
        name: "mkdir",
        arity: 1,
        symbol: "lumen_os_mkdir",
        eval: |a| Ok(Value::Bool(std::fs::create_dir(str_arg(a, 0)).is_ok())),
    },
    ModuleFn {
        module: "os",
        name: "listdir",
        arity: 1,
        symbol: "lumen_os_listdir",
        eval: |a| {
            match std::fs::read_dir(str_arg(a, 0)) {
                Ok(rd) => {
                    let mut names: Vec<String> = rd
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect();
                    names.sort();
                    Ok(list_val(names.into_iter().map(str_val).collect()))
                }
                Err(_) => Ok(Value::Nil),
            }
        },
    },
    ModuleFn {
        module: "os",
        name: "getenv",
        arity: 1,
        symbol: "lumen_os_getenv",
        eval: |a| {
            Ok(match std::env::var(str_arg(a, 0)) {
                Ok(v) => str_val(v),
                Err(_) => Value::Nil,
            })
        },
    },
    ModuleFn {
        module: "os",
        name: "setenv",
        arity: 2,
        symbol: "lumen_os_setenv",
        eval: |a| {

            std::env::set_var(str_arg(a, 0), str_arg(a, 1));
            Ok(Value::Bool(true))
        },
    },
    ModuleFn {
        module: "os",
        name: "cwd",
        arity: 0,
        symbol: "lumen_os_cwd",
        eval: |_| {
            Ok(match std::env::current_dir() {
                Ok(p) => str_val(p.to_string_lossy().into_owned()),
                Err(_) => Value::Nil,
            })
        },
    },
    ModuleFn {
        module: "os",
        name: "time",
        arity: 0,
        symbol: "lumen_os_time",

        eval: |_| {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Ok(Value::Int(secs))
        },
    },
    ModuleFn {
        module: "os",
        name: "clock",
        arity: 0,
        symbol: "lumen_os_clock",

        eval: |_| {
            use std::sync::OnceLock;
            use std::time::Instant;
            static START: OnceLock<Instant> = OnceLock::new();
            let start = START.get_or_init(Instant::now);
            Ok(Value::Int(start.elapsed().as_millis() as i64))
        },
    },
    ModuleFn {
        module: "os",
        name: "getpid",
        arity: 0,
        symbol: "lumen_os_getpid",
        eval: |_| Ok(Value::Int(std::process::id() as i64)),
    },
    ModuleFn {
        module: "os",
        name: "sep",
        arity: 0,
        symbol: "lumen_os_sep",
        eval: |_| Ok(str_val(std::path::MAIN_SEPARATOR.to_string())),
    },
    ModuleFn {
        module: "os",
        name: "platform",
        arity: 0,
        symbol: "lumen_os_platform",
        eval: |_| {
            let p = if cfg!(target_os = "windows") {
                "windows"
            } else if cfg!(target_os = "macos") {
                "macos"
            } else {
                "linux"
            };
            Ok(str_val(p.to_string()))
        },
    },

    ModuleFn {
        module: "os",
        name: "system",
        arity: 1,
        symbol: "lumen_os_system",

        eval: |a| {
            let cmd = str_arg(a, 0);
            let (sh, flag) = if cfg!(target_os = "windows") {
                ("cmd", "/C")
            } else {
                ("sh", "-c")
            };
            let code = std::process::Command::new(sh)
                .arg(flag)
                .arg(&cmd)
                .status()
                .ok()
                .and_then(|s| s.code())
                .unwrap_or(-1);
            Ok(Value::Int(code as i64))
        },
    },
    ModuleFn {
        module: "os",
        name: "exec",
        arity: 1,
        symbol: "lumen_os_exec",

        eval: |a| {
            let cmd = str_arg(a, 0);
            let (sh, flag) = if cfg!(target_os = "windows") {
                ("cmd", "/C")
            } else {
                ("sh", "-c")
            };
            Ok(
                match std::process::Command::new(sh).arg(flag).arg(&cmd).output() {
                    Ok(out) => str_val(String::from_utf8_lossy(&out.stdout).into_owned()),
                    Err(_) => Value::Nil,
                },
            )
        },
    },
    ModuleFn {
        module: "os",
        name: "exit",
        arity: 1,
        symbol: "lumen_os_exit",

        eval: |a| {
            let code = match a.first() {
                Some(Value::Int(n)) => *n as i32,
                _ => 0,
            };
            std::process::exit(code);
        },
    },
    ModuleFn {
        module: "os",
        name: "args",
        arity: 0,
        symbol: "lumen_os_args",

        eval: |_| Ok(list_val(std::env::args().map(str_val).collect())),
    },

    ModuleFn {
        module: "rand",
        name: "seed",
        arity: 1,
        symbol: "lumen_rand_seed",
        eval: |a| {
            let s = match a.first() {
                Some(Value::Int(n)) => *n as u64,
                _ => 0,
            };
            rng_set(s);
            Ok(Value::Nil)
        },
    },
    ModuleFn {
        module: "rand",
        name: "int",
        arity: 2,
        symbol: "lumen_rand_int",
        eval: |a| {
            let lo = match a.first() {
                Some(Value::Int(n)) => *n,
                _ => 0,
            };
            let hi = match a.get(1) {
                Some(Value::Int(n)) => *n,
                _ => 0,
            };
            if hi < lo {
                return Ok(Value::Int(lo));
            }
            let span = (hi - lo) as u64 + 1;
            let r = (rng_next() % span) as i64;
            Ok(Value::Int(lo + r))
        },
    },
    ModuleFn {
        module: "rand",
        name: "float",
        arity: 0,
        symbol: "lumen_rand_float",
        eval: |_| {
            let r = rng_next() >> 11;
            Ok(Value::Float(r as f64 / 9007199254740992.0))
        },
    },

    ModuleFn {
        module: "time",
        name: "now",
        arity: 0,
        symbol: "lumen_time_now",

        eval: |_| {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Ok(Value::Int(ms))
        },
    },
    ModuleFn {
        module: "time",
        name: "format",
        arity: 1,
        symbol: "lumen_time_format",

        eval: |a| Ok(str_val(fmt_epoch_utc(as_f(arg(a, 0)?)? as i64))),
    },
    ModuleFn {
        module: "time",
        name: "sleep",
        arity: 1,
        symbol: "lumen_time_sleep",

        eval: |a| {
            let ms = as_f(arg(a, 0)?)? as i64;
            if ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(ms as u64));
            }
            Ok(Value::Nil)
        },
    },

    ModuleFn {
        module: "json",
        name: "stringify",
        arity: 1,
        symbol: "lumen_json_stringify",
        eval: |a| {
            let mut out = String::new();
            json_write(a.first().unwrap_or(&Value::Nil), &mut out);
            Ok(str_val(out))
        },
    },
    ModuleFn {
        module: "json",
        name: "parse",
        arity: 1,
        symbol: "lumen_json_parse",
        eval: |a| {
            let s = str_arg(a, 0);
            let bytes = s.as_bytes();
            let mut pos = 0usize;
            match json_parse_value(bytes, &mut pos) {
                Some(v) => {
                    json_skip_ws(bytes, &mut pos);
                    if pos == bytes.len() {
                        Ok(v)
                    } else {
                        Ok(Value::Nil)
                    }
                }
                None => Ok(Value::Nil),
            }
        },
    },

    ModuleFn {
        module: "cffi",
        name: "cbuf",
        arity: 1,
        symbol: "lumen_cbuf",
        eval: |a| {
            let n = int_of(arg(a, 0)?);
            if n < 0 {
                return Err("cbuf: size must be >= 0".into());
            }
            Ok(Value::CBuf(std::rc::Rc::new(std::cell::RefCell::new(
                vec![0u8; n as usize],
            ))))
        },
    },
    ModuleFn {
        module: "cffi",
        name: "len",
        arity: 1,
        symbol: "lumen_cbuf_len",
        eval: |a| Ok(Value::Int(cbuf_of(arg(a, 0)?)?.borrow().len() as i64)),
    },
    ModuleFn {
        module: "cffi",
        name: "addr",
        arity: 1,
        symbol: "lumen_cbuf_addr",
        eval: |a| {
            let b = cbuf_of(arg(a, 0)?)?;
            let ptr = b.borrow().as_ptr() as i64;
            Ok(Value::Int(ptr))
        },
    },

    ModuleFn {
        module: "cffi",
        name: "set_i8",
        arity: 3,
        symbol: "lumen_cset_i8",
        eval: |a| {
            cset(a, 1, "set_i8", |s| {
                s.copy_from_slice(&(int_of(&a[2]) as i8).to_le_bytes())
            })
        },
    },
    ModuleFn {
        module: "cffi",
        name: "set_i16",
        arity: 3,
        symbol: "lumen_cset_i16",
        eval: |a| {
            cset(a, 2, "set_i16", |s| {
                s.copy_from_slice(&(int_of(&a[2]) as i16).to_le_bytes())
            })
        },
    },
    ModuleFn {
        module: "cffi",
        name: "set_i32",
        arity: 3,
        symbol: "lumen_cset_i32",
        eval: |a| {
            cset(a, 4, "set_i32", |s| {
                s.copy_from_slice(&(int_of(&a[2]) as i32).to_le_bytes())
            })
        },
    },
    ModuleFn {
        module: "cffi",
        name: "set_i64",
        arity: 3,
        symbol: "lumen_cset_i64",
        eval: |a| {
            cset(a, 8, "set_i64", |s| {
                s.copy_from_slice(&int_of(&a[2]).to_le_bytes())
            })
        },
    },
    ModuleFn {
        module: "cffi",
        name: "set_ptr",
        arity: 3,
        symbol: "lumen_cset_ptr",
        eval: |a| {
            cset(a, 8, "set_ptr", |s| {
                s.copy_from_slice(&int_of(&a[2]).to_le_bytes())
            })
        },
    },
    ModuleFn {
        module: "cffi",
        name: "set_f32",
        arity: 3,
        symbol: "lumen_cset_f32",
        eval: |a| {
            cset(a, 4, "set_f32", |s| {
                s.copy_from_slice(&(float_of(&a[2]) as f32).to_le_bytes())
            })
        },
    },
    ModuleFn {
        module: "cffi",
        name: "set_f64",
        arity: 3,
        symbol: "lumen_cset_f64",
        eval: |a| {
            cset(a, 8, "set_f64", |s| {
                s.copy_from_slice(&float_of(&a[2]).to_le_bytes())
            })
        },
    },

    ModuleFn {
        module: "cffi",
        name: "get_i8",
        arity: 2,
        symbol: "lumen_cget_i8",
        eval: |a| {
            let (b, _) = cget_bytes(a, 1, "get_i8")?;
            Ok(Value::Int(i8::from_le_bytes([b[0]]) as i64))
        },
    },
    ModuleFn {
        module: "cffi",
        name: "get_i16",
        arity: 2,
        symbol: "lumen_cget_i16",
        eval: |a| {
            let (b, _) = cget_bytes(a, 2, "get_i16")?;
            Ok(Value::Int(i16::from_le_bytes([b[0], b[1]]) as i64))
        },
    },
    ModuleFn {
        module: "cffi",
        name: "get_i32",
        arity: 2,
        symbol: "lumen_cget_i32",
        eval: |a| {
            let (b, _) = cget_bytes(a, 4, "get_i32")?;
            Ok(Value::Int(
                i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64
            ))
        },
    },
    ModuleFn {
        module: "cffi",
        name: "get_i64",
        arity: 2,
        symbol: "lumen_cget_i64",
        eval: |a| {
            let (b, _) = cget_bytes(a, 8, "get_i64")?;
            let mut x = [0u8; 8];
            x.copy_from_slice(&b);
            Ok(Value::Int(i64::from_le_bytes(x)))
        },
    },
    ModuleFn {
        module: "cffi",
        name: "get_ptr",
        arity: 2,
        symbol: "lumen_cget_ptr",
        eval: |a| {
            let (b, _) = cget_bytes(a, 8, "get_ptr")?;
            let mut x = [0u8; 8];
            x.copy_from_slice(&b);
            Ok(Value::Int(i64::from_le_bytes(x)))
        },
    },
    ModuleFn {
        module: "cffi",
        name: "get_f32",
        arity: 2,
        symbol: "lumen_cget_f32",
        eval: |a| {
            let (b, _) = cget_bytes(a, 4, "get_f32")?;
            Ok(Value::Float(
                f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64
            ))
        },
    },
    ModuleFn {
        module: "cffi",
        name: "get_f64",
        arity: 2,
        symbol: "lumen_cget_f64",
        eval: |a| {
            let (b, _) = cget_bytes(a, 8, "get_f64")?;
            let mut x = [0u8; 8];
            x.copy_from_slice(&b);
            Ok(Value::Float(f64::from_le_bytes(x)))
        },
    },

    ModuleFn {
        module: "cffi",
        name: "vcall",
        arity: 4,
        symbol: "lumen_com_vcall",
        eval: |a| {
            let obj = int_of(arg(a, 0)?);
            let slot = int_of(arg(a, 1)?);
            let args: Vec<u8> = match arg(a, 2)? {
                Value::CBuf(b) => b.borrow().clone(),
                Value::Nil => Vec::new(),
                _ => return Err("vcall: args must be a cbuf or nil".into()),
            };
            let ret_kind = int_of(arg(a, 3)?);
            com_vcall_impl(obj, slot, &args, ret_kind)
        },
    },

    ModuleFn {
        module: "cffi",
        name: "peek_i64",
        arity: 1,
        symbol: "lumen_peek_i64",
        eval: |a| {
            let addr = int_of(arg(a, 0)?);
            Ok(Value::Int(unsafe { *(addr as usize as *const i64) }))
        },
    },
    ModuleFn {
        module: "cffi",
        name: "poke_i64",
        arity: 2,
        symbol: "lumen_poke_i64",
        eval: |a| {
            let addr = int_of(arg(a, 0)?);
            let v = int_of(arg(a, 1)?);
            unsafe { *(addr as usize as *mut i64) = v };
            Ok(Value::Nil)
        },
    },
    ModuleFn {
        module: "cffi",
        name: "peek_i32",
        arity: 1,
        symbol: "lumen_peek_i32",
        eval: |a| {
            let addr = int_of(arg(a, 0)?);
            Ok(Value::Int(unsafe { *(addr as usize as *const i32) } as i64))
        },
    },
    ModuleFn {
        module: "cffi",
        name: "poke_i32",
        arity: 2,
        symbol: "lumen_poke_i32",
        eval: |a| {
            let addr = int_of(arg(a, 0)?);
            let v = int_of(arg(a, 1)?) as i32;
            unsafe { *(addr as usize as *mut i32) = v };
            Ok(Value::Nil)
        },
    },
    ModuleFn {
        module: "cffi",
        name: "str_ptr",
        arity: 1,
        symbol: "lumen_str_ptr",
        eval: |a| match arg(a, 0)? {
            Value::Str(s) => Ok(Value::Int(s.as_ptr() as i64)),
            _ => Ok(Value::Int(0)),
        },
    },
    ModuleFn {
        module: "cffi",
        name: "guid",
        arity: 1,
        symbol: "lumen_guid",
        eval: |a| {
            let s = match arg(a, 0)? {
                Value::Str(s) => s.to_string(),
                _ => return Err("guid: expected a string".into()),
            };

            let nibs: Vec<u8> = s
                .chars()
                .filter_map(|c| c.to_digit(16).map(|d| d as u8))
                .collect();
            if nibs.len() != 32 {
                return Err("guid: expected 32 hex digits".into());
            }
            let mut b = [0u8; 16];
            for i in 0..16 {
                b[i] = (nibs[i * 2] << 4) | nibs[i * 2 + 1];
            }

            let d = vec![
                b[3], b[2], b[1], b[0],
                b[5], b[4],
                b[7], b[6],
                b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15],
            ];
            Ok(Value::CBuf(std::rc::Rc::new(std::cell::RefCell::new(d))))
        },
    },

    ModuleFn {
        module: "cffi",
        name: "callback",
        arity: 1,
        symbol: "lumen_cb_register",
        eval: |a| match arg(a, 0)? {
            Value::Func(..) => Err(
                "callback: C callbacks require a compiled program - use `lumen build` \
                 (the interpreter has no native code pointer for a Lumen function)"
                    .into(),
            ),
            _ => Err("callback: argument must be a function".into()),
        },
    },
    // net: Winsock2 TCP/UDP sockets. A socket is an int handle (-1 = error).
    // Native side: lumen_net_* in lumen_rt.c. Windows-only.
    ModuleFn { module: "net", name: "listen", arity: 2, symbol: "lumen_net_listen", eval: |a| crate::net::listen(a) },
    ModuleFn { module: "net", name: "accept", arity: 1, symbol: "lumen_net_accept", eval: |a| crate::net::accept(a) },
    ModuleFn { module: "net", name: "connect", arity: 2, symbol: "lumen_net_connect", eval: |a| crate::net::connect(a) },
    ModuleFn { module: "net", name: "udp", arity: 2, symbol: "lumen_net_udp", eval: |a| crate::net::udp(a) },
    ModuleFn { module: "net", name: "send", arity: 2, symbol: "lumen_net_send", eval: |a| crate::net::send(a) },
    ModuleFn { module: "net", name: "recv", arity: 2, symbol: "lumen_net_recv", eval: |a| crate::net::recv(a) },
    ModuleFn { module: "net", name: "sendto", arity: 4, symbol: "lumen_net_sendto", eval: |a| crate::net::sendto(a) },
    ModuleFn { module: "net", name: "recvfrom", arity: 2, symbol: "lumen_net_recvfrom", eval: |a| crate::net::recvfrom(a) },
    ModuleFn { module: "net", name: "close", arity: 1, symbol: "lumen_net_close", eval: |a| crate::net::close(a) },
    ModuleFn { module: "net", name: "shutdown", arity: 2, symbol: "lumen_net_shutdown", eval: |a| crate::net::shutdown(a) },
    ModuleFn { module: "net", name: "set_timeout", arity: 2, symbol: "lumen_net_set_timeout", eval: |a| crate::net::set_timeout(a) },
    ModuleFn { module: "net", name: "set_blocking", arity: 2, symbol: "lumen_net_set_blocking", eval: |a| crate::net::set_blocking(a) },
    ModuleFn { module: "net", name: "set_opt", arity: 3, symbol: "lumen_net_set_opt", eval: |a| crate::net::set_opt(a) },
    ModuleFn { module: "net", name: "poll", arity: 2, symbol: "lumen_net_poll", eval: |a| crate::net::poll(a) },
    ModuleFn { module: "net", name: "resolve", arity: 1, symbol: "lumen_net_resolve", eval: |a| crate::net::resolve(a) },
    ModuleFn { module: "net", name: "local_port", arity: 1, symbol: "lumen_net_local_port", eval: |a| crate::net::local_port(a) },
    ModuleFn { module: "net", name: "errno", arity: 0, symbol: "lumen_net_errno", eval: |a| crate::net::errno(a) },
];

pub fn is_module(name: &str) -> bool {
    MODULE_FUNCS.iter().any(|f| f.module == name)
}

pub fn lookup(module: &str, name: &str) -> Option<&'static ModuleFn> {
    MODULE_FUNCS
        .iter()
        .find(|f| f.module == module && f.name == name)
}

#[cfg(test)]
mod tests {
    use super::{lookup, Value};

    #[test]
    fn math_fns_ok() {
        let f = lookup("math", "log2").expect("math.log2 missing");
        assert!(
            matches!((f.eval)(&[Value::Float(8.0)]).unwrap(), Value::Float(x) if (x - 3.0).abs() < 1e-12)
        );
        let h = lookup("math", "hypot").expect("math.hypot missing");
        assert!(
            matches!((h.eval)(&[Value::Float(3.0), Value::Float(4.0)]).unwrap(), Value::Float(x) if (x - 5.0).abs() < 1e-12)
        );
        assert!(lookup("math", "tau").is_some());
        assert!(lookup("math", "atan2").is_some());
        assert!(lookup("math", "round").is_some());

        let gcd = lookup("math", "gcd").expect("math.gcd missing");
        assert!(matches!(
            (gcd.eval)(&[Value::Int(48), Value::Int(36)]).unwrap(),
            Value::Int(12)
        ));
        assert!(matches!(
            (gcd.eval)(&[Value::Int(-12), Value::Int(8)]).unwrap(),
            Value::Int(4)
        ));
        let lcm = lookup("math", "lcm").expect("math.lcm missing");
        assert!(matches!(
            (lcm.eval)(&[Value::Int(4), Value::Int(6)]).unwrap(),
            Value::Int(12)
        ));
        assert!(matches!(
            (lcm.eval)(&[Value::Int(0), Value::Int(5)]).unwrap(),
            Value::Int(0)
        ));
        let fac = lookup("math", "factorial").expect("math.factorial missing");
        assert!(matches!(
            (fac.eval)(&[Value::Int(5)]).unwrap(),
            Value::Int(120)
        ));
        assert!(matches!(
            (fac.eval)(&[Value::Int(-3)]).unwrap(),
            Value::Int(0)
        ));
        for n in ["fmod", "copysign", "log1p", "expm1", "isfinite"] {
            assert!(lookup("math", n).is_some(), "math.{n} missing");
        }
    }

    #[test]
    fn os_fns_ok() {
        for name in [
            "setenv", "cwd", "time", "clock", "getpid", "sep", "platform", "system", "exec",
            "exit", "args",
        ] {
            assert!(lookup("os", name).is_some(), "os.{name} not registered");
        }
    }

    #[test]
    fn net_fns_ok() {
        for (name, arity) in [
            ("listen", 2u8), ("accept", 1), ("connect", 2), ("udp", 2),
            ("send", 2), ("recv", 2), ("sendto", 4), ("recvfrom", 2),
            ("close", 1), ("shutdown", 2), ("set_timeout", 2), ("set_blocking", 2),
            ("set_opt", 3), ("poll", 2), ("resolve", 1), ("local_port", 1), ("errno", 0),
        ] {
            let f = lookup("net", name).unwrap_or_else(|| panic!("net.{name} missing"));
            assert_eq!(f.arity, arity, "net.{name} arity");
        }
    }

    #[test]
    fn rand_seedable() {

        let seed = lookup("rand", "seed").expect("rand.seed missing");
        let rint = lookup("rand", "int").expect("rand.int missing");
        assert!(lookup("rand", "float").is_some());
        let roll = |args: &[Value]| match (rint.eval)(args).unwrap() {
            Value::Int(n) => n,
            _ => panic!("rand.int did not return an int"),
        };
        (seed.eval)(&[Value::Int(42)]).unwrap();
        let a1 = roll(&[Value::Int(1), Value::Int(1000)]);
        let a2 = roll(&[Value::Int(1), Value::Int(1000)]);
        (seed.eval)(&[Value::Int(42)]).unwrap();
        let b1 = roll(&[Value::Int(1), Value::Int(1000)]);
        let b2 = roll(&[Value::Int(1), Value::Int(1000)]);
        assert_eq!((a1, a2), (b1, b2), "same seed must reproduce the sequence");

        for _ in 0..1000 {
            let r = roll(&[Value::Int(5), Value::Int(7)]);
            assert!((5..=7).contains(&r), "out of range: {r}");
        }
    }

    #[test]
    fn json_roundtrip() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let strn = lookup("json", "stringify").expect("json.stringify missing");
        let parse = lookup("json", "parse").expect("json.parse missing");

        let list = Value::List(Rc::new(RefCell::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ])));
        let out = (strn.eval)(&[list]).unwrap();
        assert!(matches!(&out, Value::Str(s) if s.as_str() == "[1,2,3]"));

        let parsed =
            (parse.eval)(&[Value::Str(Rc::new("{\"a\": 1, \"b\": [2, 3]}".to_string()))]).unwrap();
        let re = (strn.eval)(&[parsed]).unwrap();
        assert!(matches!(&re, Value::Str(s) if s.as_str() == "{\"a\":1,\"b\":[2,3]}"));

        assert!(matches!(
            (parse.eval)(&[Value::Str(Rc::new("{nope".to_string()))]).unwrap(),
            Value::Nil
        ));
    }

    #[test]
    fn time_fmt_utc() {
        let fmt = lookup("time", "format").expect("time.format missing");
        let f = |secs: i64| match (fmt.eval)(&[Value::Int(secs)]).unwrap() {
            Value::Str(s) => s.to_string(),
            _ => panic!("time.format did not return a string"),
        };
        assert_eq!(f(0), "1970-01-01 00:00:00");
        assert_eq!(f(1_700_000_000), "2023-11-14 22:13:20");

        assert_eq!(f(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(f(1_582_934_400), "2020-02-29 00:00:00");
        assert!(lookup("time", "now").is_some());
        assert!(lookup("time", "sleep").is_some());
    }

    #[test]
    fn symbols_unique() {

        let mut syms: Vec<&str> = super::MODULE_FUNCS.iter().map(|f| f.symbol).collect();
        let n = syms.len();
        syms.sort_unstable();
        syms.dedup();
        assert_eq!(syms.len(), n, "duplicate runtime symbol in MODULE_FUNCS");
    }
}
