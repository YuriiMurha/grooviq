fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mode = args.next();
    if args.next().is_some() {
        return Err("usage: ipc-contract <check|update>".into());
    }
    match mode.as_deref() {
        Some("check") => grooviq_app::ipc_contract::check(),
        Some("update") => grooviq_app::ipc_contract::update(),
        _ => Err("usage: ipc-contract <check|update>".into()),
    }
}
