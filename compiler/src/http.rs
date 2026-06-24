//! Internal WinHTTP client used by the package manager (`lumen install`,
//! `lumen update`) to download files. NOT exposed to Lumen programs - the
//! language-level networking module is `net` (TCP/UDP). Windows-only.

#[cfg(windows)]
mod win {
    use std::os::raw::c_void;

    type H = *mut c_void;

    #[link(name = "winhttp")]
    extern "system" {
        fn WinHttpOpen(
            agent: *const u16,
            atype: u32,
            proxy: *const u16,
            byp: *const u16,
            fl: u32,
        ) -> H;
        fn WinHttpConnect(ses: H, host: *const u16, port: u16, res: u32) -> H;
        fn WinHttpOpenRequest(
            con: H,
            verb: *const u16,
            path: *const u16,
            ver: *const u16,
            refr: *const u16,
            acc: *const *const u16,
            fl: u32,
        ) -> H;
        fn WinHttpSendRequest(
            req: H,
            hdr: *const u16,
            hlen: u32,
            body: *const c_void,
            blen: u32,
            tot: u32,
            ctx: usize,
        ) -> i32;
        fn WinHttpReceiveResponse(req: H, res: *mut c_void) -> i32;
        fn WinHttpQueryHeaders(
            req: H,
            info: u32,
            name: *const u16,
            buf: *mut c_void,
            blen: *mut u32,
            idx: *mut u32,
        ) -> i32;
        fn WinHttpQueryDataAvailable(req: H, av: *mut u32) -> i32;
        fn WinHttpReadData(req: H, buf: *mut c_void, len: u32, got: *mut u32) -> i32;
        fn WinHttpCloseHandle(h: H) -> i32;
        fn WinHttpCrackUrl(url: *const u16, len: u32, fl: u32, uc: *mut UrlComp) -> i32;
    }

    // Matches Win32 URL_COMPONENTS (repr(C) handles padding).
    #[repr(C)]
    struct UrlComp {
        size: u32,
        scheme: *mut u16,
        scheme_len: u32,
        n_scheme: i32,
        host: *mut u16,
        host_len: u32,
        port: u16,
        user: *mut u16,
        user_len: u32,
        pass: *mut u16,
        pass_len: u32,
        path: *mut u16,
        path_len: u32,
        extra: *mut u16,
        extra_len: u32,
    }

    const AUTO_PROXY: u32 = 4; // WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
    const FLAG_SECURE: u32 = 0x0080_0000;
    const SCHEME_HTTPS: i32 = 2; // INTERNET_SCHEME_HTTPS
    const Q_STATUS: u32 = 19 | 0x2000_0000; // STATUS_CODE | FLAG_NUMBER

    fn wide(s: &str) -> Vec<u16> {
        let mut v: Vec<u16> = s.encode_utf16().collect();
        v.push(0);
        v
    }

    // GET a URL; returns (status, body). Never panics - failures map to status 0.
    pub fn get(url: &str) -> (i64, Vec<u8>) {
        unsafe {
            let wurl = wide(url);
            let mut host = [0u16; 256];
            let mut path = [0u16; 4096];
            let mut uc: UrlComp = std::mem::zeroed();
            uc.size = std::mem::size_of::<UrlComp>() as u32;
            uc.host = host.as_mut_ptr();
            uc.host_len = 255;
            uc.path = path.as_mut_ptr();
            uc.path_len = 4095;
            if WinHttpCrackUrl(wurl.as_ptr(), 0, 0, &mut uc) == 0 {
                return (0, Vec::new());
            }
            let agent = wide("lumen-pkg/1.0");
            let ses = WinHttpOpen(
                agent.as_ptr(),
                AUTO_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            );
            if ses.is_null() {
                return (0, Vec::new());
            }
            let con = WinHttpConnect(ses, host.as_ptr(), uc.port, 0);
            let sec = if uc.n_scheme == SCHEME_HTTPS {
                FLAG_SECURE
            } else {
                0
            };
            let wverb = wide("GET");
            let req = WinHttpOpenRequest(
                con,
                wverb.as_ptr(),
                path.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                sec,
            );
            let mut ok =
                WinHttpSendRequest(req, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) != 0;
            if ok {
                ok = WinHttpReceiveResponse(req, std::ptr::null_mut()) != 0;
            }
            let mut status: u32 = 0;
            let mut slen = 4u32;
            if ok {
                WinHttpQueryHeaders(
                    req,
                    Q_STATUS,
                    std::ptr::null(),
                    &mut status as *mut u32 as *mut c_void,
                    &mut slen,
                    std::ptr::null_mut(),
                );
            }
            let mut data = Vec::new();
            if ok {
                loop {
                    let mut avail: u32 = 0;
                    if WinHttpQueryDataAvailable(req, &mut avail) == 0 || avail == 0 {
                        break;
                    }
                    let mut buf = vec![0u8; avail as usize];
                    let mut got: u32 = 0;
                    WinHttpReadData(req, buf.as_mut_ptr() as *mut c_void, avail, &mut got);
                    data.extend_from_slice(&buf[..got as usize]);
                }
            }
            WinHttpCloseHandle(req);
            WinHttpCloseHandle(con);
            WinHttpCloseHandle(ses);
            (status as i64, data)
        }
    }
}

// Package manager helper: GET url, return raw body bytes on 2xx, else Err.
#[cfg(windows)]
pub fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let (status, data) = win::get(url);
    match status {
        0 => Err(format!("could not reach {url}")),
        s if (200..300).contains(&s) => Ok(data),
        s => Err(format!("{url} returned HTTP {s}")),
    }
}

#[cfg(not(windows))]
pub fn fetch(_url: &str) -> Result<Vec<u8>, String> {
    Err("package downloads are only supported on Windows".into())
}
