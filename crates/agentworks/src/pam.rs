//! Password checks through the host's PAM stack. libpam is loaded at runtime,
//! so builds and hosts without PAM are unaffected until PAM sign-in is enabled.
use std::path::Path;

/// Run `auth` and `account` for `user` in PAM service `service`. `confdir`
/// replaces `/etc/pam.d` (tests). Blocking: PAM modules may sleep or prompt helpers.
#[cfg(target_os = "linux")]
pub fn authenticate(
    service: &str,
    confdir: Option<&Path>,
    user: &str,
    password: &str,
    rhost: Option<&str>,
) -> Result<(), String> {
    linux::authenticate(service, confdir, user, password, rhost)
}

#[cfg(not(target_os = "linux"))]
pub fn authenticate(
    _service: &str,
    _confdir: Option<&Path>,
    _user: &str,
    _password: &str,
    _rhost: Option<&str>,
) -> Result<(), String> {
    Err(unsupported())
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> String {
    "PAM sign-in is only supported on Linux".into()
}

/// Startup check: libpam loads and the service has its own configuration, so a
/// typo never falls through to PAM's `other` service.
pub fn check_service(service: &str, confdir: Option<&Path>) -> Result<(), String> {
    if service.is_empty()
        || !service
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(format!("Invalid PAM service name {service:?}"));
    }
    #[cfg(target_os = "linux")]
    {
        linux::library()?;
        let dirs = match confdir {
            Some(dir) => vec![dir],
            None => ["/etc/pam.d", "/usr/lib/pam.d", "/usr/etc/pam.d"]
                .map(Path::new)
                .to_vec(),
        };
        if !dirs.iter().any(|dir| dir.join(service).is_file()) {
            let dirs: Vec<_> = dirs.iter().map(|d| d.display().to_string()).collect();
            return Err(format!(
                "PAM service {service:?} has no file in {}",
                dirs.join(", ")
            ));
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = confdir;
        Err(unsupported())
    }
}

/// The OS user running this server.
#[cfg(unix)]
pub fn current_user() -> Option<String> {
    // SAFETY: getpwuid returns static storage; it is copied before any other passwd call.
    unsafe {
        let pw = libc::getpwuid(libc::geteuid());
        if pw.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*pw).pw_name)
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}

#[cfg(not(unix))]
pub fn current_user() -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        ffi::{CStr, CString, c_char, c_int, c_void},
        os::unix::ffi::OsStrExt,
        path::Path,
        sync::OnceLock,
    };

    const PAM_SUCCESS: c_int = 0;
    const PAM_BUF_ERR: c_int = 5;
    const PAM_CONV_ERR: c_int = 19;
    const PAM_RHOST: c_int = 4;
    const PAM_SILENT: c_int = 0x8000;
    const PAM_DISALLOW_NULL_AUTHTOK: c_int = 0x0001;
    const PAM_PROMPT_ECHO_OFF: c_int = 1;
    const PAM_PROMPT_ECHO_ON: c_int = 2;
    const PAM_ERROR_MSG: c_int = 3;
    const PAM_TEXT_INFO: c_int = 4;

    #[repr(C)]
    struct Message {
        style: c_int,
        msg: *const c_char,
    }
    #[repr(C)]
    struct Reply {
        resp: *mut c_char,
        retcode: c_int,
    }
    type ConvFn = extern "C" fn(c_int, *mut *const Message, *mut *mut Reply, *mut c_void) -> c_int;
    #[repr(C)]
    struct Conv {
        conv: ConvFn,
        data: *mut c_void,
    }

    type Handle = *mut c_void;
    type StartFn =
        unsafe extern "C" fn(*const c_char, *const c_char, *const Conv, *mut Handle) -> c_int;
    type StartConfdirFn = unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        *const Conv,
        *const c_char,
        *mut Handle,
    ) -> c_int;
    type SetItemFn = unsafe extern "C" fn(Handle, c_int, *const c_void) -> c_int;
    type StepFn = unsafe extern "C" fn(Handle, c_int) -> c_int;
    type StrerrorFn = unsafe extern "C" fn(Handle, c_int) -> *const c_char;

    pub struct Library {
        start: StartFn,
        start_confdir: Option<StartConfdirFn>,
        set_item: SetItemFn,
        authenticate: StepFn,
        acct_mgmt: StepFn,
        end: StepFn,
        strerror: StrerrorFn,
    }

    pub fn library() -> Result<&'static Library, String> {
        static LIBRARY: OnceLock<Result<Library, String>> = OnceLock::new();
        LIBRARY.get_or_init(load).as_ref().map_err(Clone::clone)
    }

    fn load() -> Result<Library, String> {
        // SAFETY: libpam stays loaded for the process lifetime; each symbol is
        // transmuted to its documented C signature.
        unsafe {
            let handle = libc::dlopen(c"libpam.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if handle.is_null() {
                return Err("Cannot load libpam.so.0".into());
            }
            let symbol = |name: &CStr| {
                let ptr = libc::dlsym(handle, name.as_ptr());
                (!ptr.is_null()).then_some(ptr)
            };
            let required = |name: &CStr| {
                symbol(name).ok_or_else(|| format!("libpam lacks {}", name.to_string_lossy()))
            };
            use std::mem::transmute;
            Ok(Library {
                start: transmute::<Handle, StartFn>(required(c"pam_start")?),
                start_confdir: symbol(c"pam_start_confdir")
                    .map(|p| transmute::<Handle, StartConfdirFn>(p)),
                set_item: transmute::<Handle, SetItemFn>(required(c"pam_set_item")?),
                authenticate: transmute::<Handle, StepFn>(required(c"pam_authenticate")?),
                acct_mgmt: transmute::<Handle, StepFn>(required(c"pam_acct_mgmt")?),
                end: transmute::<Handle, StepFn>(required(c"pam_end")?),
                strerror: transmute::<Handle, StrerrorFn>(required(c"pam_strerror")?),
            })
        }
    }

    /// NUL-terminated bytes, zeroed on drop.
    struct Secret(Vec<u8>);
    impl Secret {
        fn new(text: &str) -> Result<Self, String> {
            if text.contains('\0') {
                return Err("Credentials contain NUL".into());
            }
            let mut bytes = Vec::with_capacity(text.len() + 1);
            bytes.extend_from_slice(text.as_bytes());
            bytes.push(0);
            Ok(Self(bytes))
        }
        fn ptr(&self) -> *const c_char {
            self.0.as_ptr().cast()
        }
    }
    impl Drop for Secret {
        fn drop(&mut self) {
            for byte in &mut self.0 {
                // SAFETY: in-bounds write; volatile so the clear is not optimized away.
                unsafe { std::ptr::write_volatile(byte, 0) };
            }
        }
    }

    struct Credentials {
        user: Secret,
        password: Secret,
    }

    extern "C" fn converse(
        count: c_int,
        messages: *mut *const Message,
        replies: *mut *mut Reply,
        data: *mut c_void,
    ) -> c_int {
        if !(1..=32).contains(&count) || messages.is_null() || replies.is_null() {
            return PAM_CONV_ERR;
        }
        let count = count as usize;
        // SAFETY: Linux-PAM passes `count` message pointers and our Credentials;
        // replies are malloc'd because PAM frees them.
        unsafe {
            let credentials = &*(data as *const Credentials);
            let out = libc::calloc(count, std::mem::size_of::<Reply>()) as *mut Reply;
            if out.is_null() {
                return PAM_BUF_ERR;
            }
            let fail = |code| {
                for i in 0..count {
                    let resp = (*out.add(i)).resp;
                    if !resp.is_null() {
                        libc::explicit_bzero(resp.cast(), libc::strlen(resp));
                        libc::free(resp.cast());
                    }
                }
                libc::free(out.cast());
                code
            };
            for i in 0..count {
                let message = &**messages.add(i);
                let answer = match message.style {
                    PAM_PROMPT_ECHO_OFF => &credentials.password,
                    PAM_PROMPT_ECHO_ON => &credentials.user,
                    PAM_ERROR_MSG | PAM_TEXT_INFO => continue,
                    _ => return fail(PAM_CONV_ERR),
                };
                let copy = libc::strdup(answer.ptr());
                if copy.is_null() {
                    return fail(PAM_BUF_ERR);
                }
                (*out.add(i)).resp = copy;
            }
            *replies = out;
            PAM_SUCCESS
        }
    }

    pub fn authenticate(
        service: &str,
        confdir: Option<&Path>,
        user: &str,
        password: &str,
        rhost: Option<&str>,
    ) -> Result<(), String> {
        let pam = library()?;
        let service = CString::new(service).map_err(|e| e.to_string())?;
        let credentials = Credentials {
            user: Secret::new(user)?,
            password: Secret::new(password)?,
        };
        let conv = Conv {
            conv: converse,
            data: &credentials as *const Credentials as *mut c_void,
        };
        let rhost = rhost
            .map(CString::new)
            .transpose()
            .map_err(|e| e.to_string())?;
        let mut handle = std::ptr::null_mut();
        // SAFETY: every pointer outlives the PAM handle, which is ended before return.
        unsafe {
            let started = match confdir {
                None => (pam.start)(service.as_ptr(), credentials.user.ptr(), &conv, &mut handle),
                Some(dir) => {
                    let start = pam.start_confdir.ok_or("libpam lacks pam_start_confdir")?;
                    let dir =
                        CString::new(dir.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
                    start(
                        service.as_ptr(),
                        credentials.user.ptr(),
                        &conv,
                        dir.as_ptr(),
                        &mut handle,
                    )
                }
            };
            if started != PAM_SUCCESS || handle.is_null() {
                return Err(format!("pam_start failed ({started})"));
            }
            let mut status = match &rhost {
                Some(rhost) => (pam.set_item)(handle, PAM_RHOST, rhost.as_ptr().cast()),
                None => PAM_SUCCESS,
            };
            if status == PAM_SUCCESS {
                status = (pam.authenticate)(handle, PAM_SILENT | PAM_DISALLOW_NULL_AUTHTOK);
            }
            if status == PAM_SUCCESS {
                status = (pam.acct_mgmt)(handle, PAM_SILENT | PAM_DISALLOW_NULL_AUTHTOK);
            }
            let result = if status == PAM_SUCCESS {
                Ok(())
            } else {
                let text = (pam.strerror)(handle, status);
                Err(if text.is_null() {
                    format!("PAM error {status}")
                } else {
                    CStr::from_ptr(text).to_string_lossy().into_owned()
                })
            };
            (pam.end)(handle, status);
            result
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Confdir(PathBuf);
    impl Confdir {
        fn new(rules: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("agentworks-pam-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("agentworks-test"), rules).unwrap();
            Self(dir)
        }
        fn check(&self, user: &str, password: &str) -> Result<(), String> {
            authenticate(
                "agentworks-test",
                Some(&self.0),
                user,
                password,
                Some("192.0.2.1"),
            )
        }
    }
    impl Drop for Confdir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn module(name: &str) -> bool {
        [
            "/usr/lib/security",
            "/lib/security",
            "/lib/x86_64-linux-gnu/security",
        ]
        .iter()
        .any(|dir| Path::new(dir).join(name).exists())
    }

    #[test]
    fn pam_stack_decides() {
        if linux::library().is_err() || !module("pam_permit.so") {
            eprintln!("skipping: libpam or pam_permit.so unavailable");
            return;
        }
        let permit = Confdir::new("auth required pam_permit.so\naccount required pam_permit.so\n");
        assert_eq!(permit.check("alice", "pw"), Ok(()));
        assert!(permit.check("al\0ice", "pw").is_err());
        let deny = Confdir::new("auth required pam_deny.so\naccount required pam_permit.so\n");
        assert!(deny.check("alice", "pw").is_err());
        let account = Confdir::new("auth required pam_permit.so\naccount required pam_deny.so\n");
        assert!(account.check("alice", "pw").is_err());
        let bob = Confdir::new(
            "auth required pam_succeed_if.so quiet user = bob\naccount required pam_permit.so\n",
        );
        assert!(bob.check("alice", "pw").is_err());
        assert_eq!(bob.check("bob", "pw"), Ok(()));
    }

    #[test]
    fn password_reaches_modules_through_the_conversation() {
        if linux::library().is_err() || !module("pam_exec.so") {
            eprintln!("skipping: libpam or pam_exec.so unavailable");
            return;
        }
        let dir = Confdir::new("");
        // expose_authtok prompts through the conversation and writes the answer to stdin.
        let script = dir.0.join("check.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nread -r pw\n[ \"$pw\" = \"correct horse\" ]\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        std::fs::write(
            dir.0.join("agentworks-test"),
            format!(
                "auth required pam_exec.so expose_authtok quiet {}\naccount required pam_permit.so\n",
                script.display()
            ),
        )
        .unwrap();
        assert_eq!(dir.check("alice", "correct horse"), Ok(()));
        assert!(dir.check("alice", "wrong").is_err());
    }

    #[test]
    fn service_names_are_validated() {
        assert!(check_service("../etc/passwd", None).is_err());
        assert!(check_service("", None).is_err());
        assert!(check_service("agentworks-surely-missing-service", None).is_err());
        if linux::library().is_ok() {
            let dir = Confdir::new("auth required pam_deny.so\n");
            assert_eq!(check_service("agentworks-test", Some(&dir.0)), Ok(()));
            assert!(check_service("login", Some(&dir.0)).is_err());
        }
    }
}
