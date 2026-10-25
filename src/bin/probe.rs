// WMI 隔离验证探针：只写标记文件，不自删（用后即删）。
fn main() {
    let roam = dirs::config_dir().unwrap();
    let path = roam.join("probe-wmi-ok.txt");
    match std::fs::write(&path, b"spawned-external") {
        Ok(_) => println!("WMI spawn write OK"),
        Err(e) => println!("WMI spawn write ERR {e}"),
    }
    let _ = std::fs::write(r"D:\rustterm-tauri\rustterm-egui\probe-result.txt", b"done\n");
}
