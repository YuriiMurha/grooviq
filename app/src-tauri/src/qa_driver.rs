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

fn read_request(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "request deadline"))?;
        // Use whole milliseconds: Darwin rejects a timeval rounded to 1,000,000 usec.
        let timeout = Duration::from_millis(remaining.as_millis().max(1) as u64);
        stream.set_read_timeout(Some(timeout))?;
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk)?;
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

pub fn start(app: tauri::AppHandle, dir: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let socket = dir.join("control.sock");
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
            let request = read_request(&mut stream);
            let bytes = request.as_ref().cloned().unwrap_or_default();
            let shutdown = bytes == b"{\"shutdown\":true}";
            if shutdown {
                let _ = stream.write_all(b"{\"ok\":true}");
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
            let _ = stream.write_all(reply.to_string().as_bytes());
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
