//! Embeds the app icon and version info into the Windows executable, so
//! Explorer, the taskbar, Start menu, Task Manager and the MSI show them.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        // Task Manager and "Open with" show FileDescription as the app name.
        res.set("FileDescription", "Outlook RS");
        res.set("ProductName", "Outlook RS");
        res.compile().expect("embedding the Windows icon resource (needs rc.exe from the Windows SDK)");
    }
}
