//! Windows networking (Winsock2) for the interpreter: the `net` builtin module.
//! TCP + UDP sockets plus the options advanced code needs (timeouts, blocking
//! mode, setsockopt, select-based poll, name resolution). The native backend
//! mirrors every function in lumen_rt.c (lumen_net_*) so both are identical.
//!
//! A socket is a Lumen int (the OS handle); -1 means error. Data is text
//! (UTF-8, same convention as os.read - use cffi for raw binary). recvfrom
//! returns a map {data, host, port}.

use crate::interp::Value;
use std::cell::RefCell;
use std::rc::Rc;

fn ival(v: &Value) -> i64 {
    match v {
        Value::Int(n) => *n,
        Value::Bool(b) => *b as i64,
        Value::Float(x) => *x as i64,
        _ => 0,
    }
}
fn sval(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        _ => String::new(),
    }
}
fn arg(a: &[Value], i: usize) -> &Value {
    a.get(i).unwrap_or(&Value::Nil)
}

#[cfg(windows)]
mod sys {
    use std::os::raw::{c_char, c_int, c_void};
    use std::sync::Once;

    pub type Socket = usize;
    pub const INVALID: Socket = usize::MAX;

    pub const AF_INET: c_int = 2;
    pub const SOCK_STREAM: c_int = 1;
    pub const SOCK_DGRAM: c_int = 2;
    pub const IPPROTO_TCP: c_int = 6;
    pub const IPPROTO_UDP: c_int = 17;
    pub const SOL_SOCKET: c_int = 0xffff;
    pub const SO_REUSEADDR: c_int = 0x0004;
    pub const SO_KEEPALIVE: c_int = 0x0008;
    pub const SO_BROADCAST: c_int = 0x0020;
    pub const SO_SNDBUF: c_int = 0x1001;
    pub const SO_RCVBUF: c_int = 0x1002;
    pub const SO_SNDTIMEO: c_int = 0x1005;
    pub const SO_RCVTIMEO: c_int = 0x1006;
    pub const TCP_NODELAY: c_int = 0x0001;
    pub const FIONBIO: i32 = -2147195266; // 0x8004667E
    pub const SOCKET_ERROR: c_int = -1;

    #[repr(C)]
    pub struct SockAddrIn {
        pub family: u16,
        pub port: u16,   // network byte order
        pub addr: u32,   // network byte order
        pub zero: [u8; 8],
    }

    #[repr(C)]
    pub struct AddrInfo {
        pub flags: c_int,
        pub family: c_int,
        pub socktype: c_int,
        pub protocol: c_int,
        pub addrlen: usize,
        pub canonname: *mut c_char,
        pub addr: *mut SockAddrIn,
        pub next: *mut AddrInfo,
    }

    #[repr(C)]
    pub struct TimeVal {
        pub sec: i32,
        pub usec: i32,
    }

    #[repr(C)]
    pub struct FdSet {
        pub count: u32,
        pub array: [Socket; 64],
    }

    #[link(name = "ws2_32")]
    extern "system" {
        pub fn WSAStartup(ver: u16, data: *mut c_void) -> c_int;
        pub fn WSAGetLastError() -> c_int;
        pub fn socket(af: c_int, ty: c_int, proto: c_int) -> Socket;
        pub fn bind(s: Socket, addr: *const SockAddrIn, len: c_int) -> c_int;
        pub fn listen(s: Socket, backlog: c_int) -> c_int;
        pub fn accept(s: Socket, addr: *mut SockAddrIn, len: *mut c_int) -> Socket;
        pub fn connect(s: Socket, addr: *const SockAddrIn, len: c_int) -> c_int;
        pub fn send(s: Socket, buf: *const c_char, len: c_int, flags: c_int) -> c_int;
        pub fn recv(s: Socket, buf: *mut c_char, len: c_int, flags: c_int) -> c_int;
        pub fn sendto(s: Socket, buf: *const c_char, len: c_int, flags: c_int, to: *const SockAddrIn, tolen: c_int) -> c_int;
        pub fn recvfrom(s: Socket, buf: *mut c_char, len: c_int, flags: c_int, from: *mut SockAddrIn, fromlen: *mut c_int) -> c_int;
        pub fn closesocket(s: Socket) -> c_int;
        pub fn shutdown(s: Socket, how: c_int) -> c_int;
        pub fn setsockopt(s: Socket, level: c_int, opt: c_int, val: *const c_char, len: c_int) -> c_int;
        pub fn getsockname(s: Socket, addr: *mut SockAddrIn, len: *mut c_int) -> c_int;
        pub fn ioctlsocket(s: Socket, cmd: i32, arg: *mut u32) -> c_int;
        pub fn select(nfds: c_int, rd: *mut FdSet, wr: *mut FdSet, ex: *mut FdSet, tv: *const TimeVal) -> c_int;
        pub fn getaddrinfo(node: *const c_char, svc: *const c_char, hints: *const AddrInfo, res: *mut *mut AddrInfo) -> c_int;
        pub fn freeaddrinfo(ai: *mut AddrInfo);
    }

    static INIT: Once = Once::new();
    pub fn startup() {
        INIT.call_once(|| unsafe {
            let mut data = [0u8; 512]; // WSADATA
            WSAStartup(0x0202, data.as_mut_ptr() as *mut c_void);
        });
    }

    pub fn htons(p: u16) -> u16 {
        p.to_be()
    }
    pub fn ntohs(p: u16) -> u16 {
        u16::from_be(p)
    }

    fn cstr(s: &str) -> std::ffi::CString {
        std::ffi::CString::new(s).unwrap_or_else(|_| std::ffi::CString::new("").unwrap())
    }

    // Resolve host (dotted IP or name) + port into a sockaddr_in. host "" =
    // INADDR_ANY (for bind). Returns None on failure.
    pub fn resolve_addr(host: &str, port: u16) -> Option<SockAddrIn> {
        let mut sa = SockAddrIn {
            family: AF_INET as u16,
            port: htons(port),
            addr: 0,
            zero: [0; 8],
        };
        if host.is_empty() {
            return Some(sa); // INADDR_ANY = 0
        }
        unsafe {
            let hints = AddrInfo {
                flags: 0,
                family: AF_INET,
                socktype: 0,
                protocol: 0,
                addrlen: 0,
                canonname: std::ptr::null_mut(),
                addr: std::ptr::null_mut(),
                next: std::ptr::null_mut(),
            };
            let node = cstr(host);
            let mut res: *mut AddrInfo = std::ptr::null_mut();
            if getaddrinfo(node.as_ptr(), std::ptr::null(), &hints, &mut res) != 0 || res.is_null() {
                return None;
            }
            let ai = &*res;
            if !ai.addr.is_null() {
                sa.addr = (*ai.addr).addr;
            }
            freeaddrinfo(res);
            Some(sa)
        }
    }

    // a.b.c.d from a network-order u32.
    pub fn ip_string(addr: u32) -> String {
        let o = addr.to_ne_bytes();
        format!("{}.{}.{}.{}", o[0], o[1], o[2], o[3])
    }

    pub fn last_error() -> i64 {
        unsafe { WSAGetLastError() as i64 }
    }
    pub use self::{
        accept as c_accept, bind as c_bind, closesocket as c_close, connect as c_connect,
        getsockname as c_getsockname, ioctlsocket as c_ioctl, listen as c_listen, recv as c_recv,
        recvfrom as c_recvfrom, select as c_select, send as c_send, sendto as c_sendto,
        setsockopt as c_setsockopt, shutdown as c_shutdown, socket as c_socket,
    };
}

#[cfg(windows)]
fn sv(s: String) -> Value {
    Value::Str(Rc::new(s))
}

// ---- public API: one fn per net.* builtin (all also exist in lumen_rt.c) ----

#[cfg(windows)]
pub fn listen(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    startup();
    let host = sval(arg(a, 0));
    let port = ival(arg(a, 1)) as u16;
    unsafe {
        let s = c_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        if s == INVALID {
            return Ok(Value::Int(-1));
        }
        let one: i32 = 1;
        c_setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one as *const i32 as *const _, 4);
        let Some(sa) = resolve_addr(&host, port) else {
            c_close(s);
            return Ok(Value::Int(-1));
        };
        if c_bind(s, &sa, 16) == SOCKET_ERROR || c_listen(s, 128) == SOCKET_ERROR {
            c_close(s);
            return Ok(Value::Int(-1));
        }
        Ok(Value::Int(s as i64))
    }
}

#[cfg(windows)]
pub fn accept(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    unsafe {
        let c = c_accept(s, std::ptr::null_mut(), std::ptr::null_mut());
        Ok(Value::Int(if c == INVALID { -1 } else { c as i64 }))
    }
}

#[cfg(windows)]
pub fn connect(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    startup();
    let host = sval(arg(a, 0));
    let port = ival(arg(a, 1)) as u16;
    unsafe {
        let s = c_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        if s == INVALID {
            return Ok(Value::Int(-1));
        }
        let Some(sa) = resolve_addr(&host, port) else {
            c_close(s);
            return Ok(Value::Int(-1));
        };
        if c_connect(s, &sa, 16) == SOCKET_ERROR {
            c_close(s);
            return Ok(Value::Int(-1));
        }
        Ok(Value::Int(s as i64))
    }
}

#[cfg(windows)]
pub fn udp(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    startup();
    let host = sval(arg(a, 0));
    let port = ival(arg(a, 1)) as u16;
    unsafe {
        let s = c_socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
        if s == INVALID {
            return Ok(Value::Int(-1));
        }
        let Some(sa) = resolve_addr(&host, port) else {
            c_close(s);
            return Ok(Value::Int(-1));
        };
        // bind only when a host or port was requested; "" + 0 = unbound client.
        if (!host.is_empty() || port != 0) && c_bind(s, &sa, 16) == SOCKET_ERROR {
            c_close(s);
            return Ok(Value::Int(-1));
        }
        Ok(Value::Int(s as i64))
    }
}

#[cfg(windows)]
pub fn send(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    let data = sval(arg(a, 1));
    unsafe {
        let n = c_send(s, data.as_ptr() as *const _, data.len() as i32, 0);
        Ok(Value::Int(n as i64))
    }
}

#[cfg(windows)]
pub fn recv(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    let max = ival(arg(a, 1)).max(0) as usize;
    let mut buf = vec![0u8; max];
    unsafe {
        let n = c_recv(s, buf.as_mut_ptr() as *mut _, max as i32, 0);
        if n < 0 {
            return Ok(Value::Nil);
        }
        Ok(sv(String::from_utf8_lossy(&buf[..n as usize]).into_owned()))
    }
}

#[cfg(windows)]
pub fn sendto(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    let data = sval(arg(a, 1));
    let host = sval(arg(a, 2));
    let port = ival(arg(a, 3)) as u16;
    unsafe {
        let Some(sa) = resolve_addr(&host, port) else {
            return Ok(Value::Int(-1));
        };
        let n = c_sendto(s, data.as_ptr() as *const _, data.len() as i32, 0, &sa, 16);
        Ok(Value::Int(n as i64))
    }
}

#[cfg(windows)]
pub fn recvfrom(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    let max = ival(arg(a, 1)).max(0) as usize;
    let mut buf = vec![0u8; max];
    unsafe {
        let mut from: SockAddrIn = std::mem::zeroed();
        let mut flen: i32 = 16;
        let n = c_recvfrom(s, buf.as_mut_ptr() as *mut _, max as i32, 0, &mut from, &mut flen);
        if n < 0 {
            return Ok(Value::Nil);
        }
        let data = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
        let e = vec![
            (sv("data".into()), sv(data)),
            (sv("host".into()), sv(ip_string(from.addr))),
            (sv("port".into()), Value::Int(ntohs(from.port) as i64)),
        ];
        Ok(Value::Map(Rc::new(RefCell::new(e))))
    }
}

#[cfg(windows)]
pub fn close(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    unsafe {
        c_close(s);
    }
    Ok(Value::Nil)
}

#[cfg(windows)]
pub fn shutdown(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    let how = ival(arg(a, 1)) as i32;
    unsafe { Ok(Value::Int(c_shutdown(s, how) as i64)) }
}

#[cfg(windows)]
pub fn set_timeout(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    let ms = ival(arg(a, 1)) as i32;
    unsafe {
        let r1 = c_setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &ms as *const i32 as *const _, 4);
        let r2 = c_setsockopt(s, SOL_SOCKET, SO_SNDTIMEO, &ms as *const i32 as *const _, 4);
        Ok(Value::Int(if r1 == 0 && r2 == 0 { 0 } else { -1 }))
    }
}

#[cfg(windows)]
pub fn set_blocking(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    let blocking = ival(arg(a, 1)) != 0;
    unsafe {
        let mut mode: u32 = if blocking { 0 } else { 1 };
        Ok(Value::Int(c_ioctl(s, FIONBIO, &mut mode) as i64))
    }
}

#[cfg(windows)]
pub fn set_opt(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    let name = sval(arg(a, 1));
    let val = ival(arg(a, 2)) as i32;
    let (level, opt) = match name.as_str() {
        "reuseaddr" => (SOL_SOCKET, SO_REUSEADDR),
        "keepalive" => (SOL_SOCKET, SO_KEEPALIVE),
        "broadcast" => (SOL_SOCKET, SO_BROADCAST),
        "sndbuf" => (SOL_SOCKET, SO_SNDBUF),
        "rcvbuf" => (SOL_SOCKET, SO_RCVBUF),
        "nodelay" => (IPPROTO_TCP, TCP_NODELAY),
        _ => return Ok(Value::Int(-1)),
    };
    unsafe {
        Ok(Value::Int(
            c_setsockopt(s, level, opt, &val as *const i32 as *const _, 4) as i64,
        ))
    }
}

#[cfg(windows)]
pub fn poll(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    let ms = ival(arg(a, 1));
    unsafe {
        let mk = || FdSet {
            count: 1,
            array: {
                let mut arr = [0usize; 64];
                arr[0] = s;
                arr
            },
        };
        let mut rd = mk();
        let mut wr = mk();
        let tv = TimeVal {
            sec: (ms / 1000) as i32,
            usec: ((ms % 1000) * 1000) as i32,
        };
        let tvp = if ms < 0 {
            std::ptr::null()
        } else {
            &tv as *const TimeVal
        };
        let r = c_select(0, &mut rd, &mut wr, std::ptr::null_mut(), tvp);
        if r < 0 {
            return Ok(Value::Int(-1));
        }
        let mut mask = 0i64;
        if (0..rd.count as usize).any(|i| rd.array[i] == s) {
            mask |= 1;
        }
        if (0..wr.count as usize).any(|i| wr.array[i] == s) {
            mask |= 2;
        }
        Ok(Value::Int(mask))
    }
}

#[cfg(windows)]
pub fn resolve(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    startup();
    let host = sval(arg(a, 0));
    match resolve_addr(&host, 0) {
        Some(sa) if sa.addr != 0 => Ok(sv(ip_string(sa.addr))),
        _ => Ok(Value::Nil),
    }
}

#[cfg(windows)]
pub fn local_port(a: &[Value]) -> Result<Value, String> {
    use sys::*;
    let s = ival(arg(a, 0)) as Socket;
    unsafe {
        let mut sa: SockAddrIn = std::mem::zeroed();
        let mut len: i32 = 16;
        if c_getsockname(s, &mut sa, &mut len) == SOCKET_ERROR {
            return Ok(Value::Int(-1));
        }
        Ok(Value::Int(ntohs(sa.port) as i64))
    }
}

#[cfg(windows)]
pub fn errno(_a: &[Value]) -> Result<Value, String> {
    Ok(Value::Int(sys::last_error()))
}

// Non-Windows: every entry errors (net is Windows-only, like cffi).
#[cfg(not(windows))]
mod stub {
    use super::Value;
    pub fn err() -> Result<Value, String> {
        Err("net: only supported on Windows".into())
    }
}

#[cfg(not(windows))]
macro_rules! stubs {
    ($($name:ident),*) => { $( pub fn $name(_a: &[Value]) -> Result<Value, String> { stub::err() } )* };
}
#[cfg(not(windows))]
stubs!(
    listen, accept, connect, udp, send, recv, sendto, recvfrom, close, shutdown, set_timeout,
    set_blocking, set_opt, poll, resolve, local_port, errno
);
