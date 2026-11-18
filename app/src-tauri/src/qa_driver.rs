//! Trusted local automation, deliberately absent from ordinary builds.
use std::{
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};
use tauri::Manager;
const LIMIT: usize = 65536;

pub fn enabled() -> bool {
    std::env::var_os("GROOVIQ_QA_DRIVER").is_some()
}

fn safe_mode(opt: Option<&str>, audio: Option<&str>) -> bool {
    opt == Some("1") && audio == Some("null")
}

pub fn validate() -> Result<PathBuf, String> {
    if !safe_mode(
        std::env::var("GROOVIQ_QA_DRIVER").ok().as_deref(),
        std::env::var("GROOVIQ_AUDIO").ok().as_deref(),
    ) {
        return Err("QA requires explicit opt-in and null audio".into());
    }
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("missing HOME")?);
    let temp = std::env::temp_dir()
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let fixed = PathBuf::from(std::env::var_os("CFFIXED_USER_HOME").ok_or("missing fixed HOME")?);
    validate_home(&home, &fixed, &temp)
}

fn validate_home(
    home: &std::path::Path,
    fixed: &std::path::Path,
    temp: &std::path::Path,
) -> Result<PathBuf, String> {
    let canonical = home.canonicalize().map_err(|e| e.to_string())?;
    let meta = std::fs::symlink_metadata(home).map_err(|e| e.to_string())?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if meta.uid() != uid
        || canonical != home
        || !home.starts_with(temp)
        || home == temp
        || !meta.is_dir()
        || meta.mode() & 0o077 != 0
        || fixed != home
    {
        return Err("QA requires a private canonical temporary HOME".into());
    }
    // Create exclusively; never replace another endpoint.
    let endpoint = home.join(".qa-driver");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&endpoint)
        .map_err(|e| e.to_string())?;
    Ok(endpoint)
}
use std::os::unix::fs::DirBuilderExt;

fn wait_ready(
    stream: &UnixStream,
    events: libc::c_short,
    deadline: Instant,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "socket deadline"))?;
        let mut fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        let milliseconds = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
        // SAFETY: one initialized pollfd for a live borrowed socket, bounded timeout.
        let ready = unsafe { libc::poll(&mut fd, 1, milliseconds) };
        if ready > 0 {
            return Ok(());
        }
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn read_request(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    loop {
        wait_ready(stream, libc::POLLIN, deadline)?;
        let mut chunk = [0; 4096];
        let count = match stream.read(&mut chunk) {
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len() + count > LIMIT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request too large",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

fn write_response_until(
    stream: &mut UnixStream,
    mut bytes: &[u8],
    deadline: Instant,
) -> std::io::Result<()> {
    // Blocking writes can renew a kernel timeout internally while making partial progress.
    stream.set_nonblocking(true)?;
    while !bytes.is_empty() {
        wait_ready(stream, libc::POLLOUT, deadline)?;
        match stream.write(bytes) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub fn start(app: tauri::AppHandle, dir: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let socket = dir.join("control.sock");
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let request = read_request(&mut stream);
            let bytes = request.as_ref().cloned().unwrap_or_default();
            let shutdown = bytes == b"{\"shutdown\":true}";
            if shutdown {
                let _ = write_response_until(
                    &mut stream,
                    b"{\"ok\":true}",
                    Instant::now() + Duration::from_secs(3),
                );
                app.exit(0);
                break;
            }
            let result = (|| -> Result<String, String> {
                request.map_err(|e| e.to_string())?;
                if bytes.len() > LIMIT {
                    return Err("request too large".into());
                }
                let req: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                let script = req["script"].as_str().ok_or("missing script")?;
                let window = app.get_webview_window("main").ok_or("missing window")?;
                let (tx, rx) = mpsc::sync_channel(1);
                window.eval_with_callback(format!("JSON.stringify((()=>{{try{{return {{ok:true,value:(()=>{{{script}}})()}}}}catch(e){{return {{ok:false,error:String(e)}}}}}})())"), move |value| { let _ = tx.try_send(value); }).map_err(|e| e.to_string())?;
                let value = rx
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|e| e.to_string())?;
                if value.len() > LIMIT {
                    return Err("response too large".into());
                }
                Ok(value)
            })();
            let id = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| v.get("id").cloned());
            let reply = serde_json::json!({"id":id,"result":result.as_ref().ok(), "error":result.as_ref().err()});
            let _ = write_response_until(
                &mut stream,
                reply.to_string().as_bytes(),
                Instant::now() + Duration::from_secs(3),
            );
        }
        let _ = std::fs::remove_file(socket);
        let _ = std::fs::remove_dir(dir);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_bytes_are_preserved() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        write_response_until(&mut writer, b"{}", Instant::now() + Duration::from_secs(1)).unwrap();
        let mut bytes = [0; 2];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"{}");
    }

    #[test]
    fn slow_reader_does_not_renew_response_deadline() {
        use std::os::fd::AsRawFd;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let size: libc::c_int = 1024;
        // SAFETY: live socket descriptor and a correctly sized integer socket option.
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    writer.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&size as *const libc::c_int).cast(),
                    std::mem::size_of_val(&size) as libc::socklen_t,
                )
            },
            0
        );
        reader
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let read = Arc::new(AtomicUsize::new(0));
        let client = std::thread::spawn({
            let (stop, read) = (Arc::clone(&stop), Arc::clone(&read));
            move || {
                let mut bytes = [0; 1024];
                while !stop.load(Ordering::Relaxed) {
                    match reader.read(&mut bytes) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            read.fetch_add(n, Ordering::Relaxed);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        });
        let result = write_response_until(
            &mut writer,
            &vec![b'x'; LIMIT],
            Instant::now() + Duration::from_millis(500),
        );
        stop.store(true, Ordering::Relaxed);
        drop(writer);
        client.join().unwrap();
        assert!(
            read.load(Ordering::Relaxed) > 1024,
            "client made partial progress"
        );
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    }
    #[test]
    fn request_limit_is_enforced_on_the_socket() {
        use std::net::Shutdown;
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        let sender = std::thread::spawn(move || {
            writer
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let _ = writer.write_all(&vec![b'x'; LIMIT + 1]);
            let _ = writer.shutdown(Shutdown::Write);
        });
        assert_eq!(
            read_request(&mut reader).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        sender.join().unwrap();
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"{}").unwrap();
        writer.shutdown(Shutdown::Write).unwrap();
        assert_eq!(read_request(&mut reader).unwrap(), b"{}");
    }
    #[test]
    fn explicit_opt_in_and_null_audio() {
        assert!(safe_mode(Some("1"), Some("null")));
        for opt in [None, Some("0"), Some("true"), Some("")] {
            assert!(!safe_mode(opt, Some("null")));
        }
        for audio in [None, Some("default"), Some("")] {
            assert!(!safe_mode(Some("1"), audio));
        }
    }
    #[test]
    fn home_and_endpoint_guards() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(validate_home(&home, &root, &root).is_err());
        assert!(validate_home(&root, &root, &root).is_err());
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_home(&home, &home, &root).is_err());
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        assert!(validate_home(&link, &link, &root).is_err());
        let endpoint = validate_home(&home, &home, &root).unwrap();
        assert_eq!(std::fs::metadata(&endpoint).unwrap().mode() & 0o777, 0o700);
        std::fs::write(endpoint.join("sentinel"), "keep").unwrap();
        assert!(validate_home(&home, &home, &root).is_err());
        assert_eq!(
            std::fs::read_to_string(endpoint.join("sentinel")).unwrap(),
            "keep"
        );
    }
}
