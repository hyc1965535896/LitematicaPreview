pub fn register() -> Result<(), String> {
    platform::register(crate::EXTENSIONS)
}

pub fn register_extensions(selection: &str) -> Result<(), String> {
    let mut selected = Vec::new();
    for value in selection.split(',') {
        let extension = crate::EXTENSIONS
            .iter()
            .copied()
            .find(|extension| extension.eq_ignore_ascii_case(value.trim()))
            .ok_or_else(|| format!("不受支持的投影文件扩展名：{value}"))?;
        if !selected.contains(&extension) {
            selected.push(extension);
        }
    }
    platform::register(&selected)
}

#[cfg(windows)]
mod platform {
    use std::{borrow::Cow, io, ptr};

    use windows_sys::Win32::UI::{
        Shell::{SHChangeNotify, ShellExecuteW, SHCNE_ASSOCCHANGED, SHCNF_IDLIST},
        WindowsAndMessaging::SW_SHOWNORMAL,
    };
    use winreg::{
        enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_NONE},
        RegKey, RegValue,
    };

    const PROG_ID: &str = "LitematicaPreview.Schematic";
    const PROG_KEY: &str = r"Software\Classes\LitematicaPreview.Schematic";
    const APPLICATION_KEY: &str = r"Software\Classes\Applications\LitematicaPreview.exe";
    const CAPABILITIES_KEY: &str = r"Software\LitematicaPreview\Capabilities";
    const REGISTERED_APPLICATIONS: &str = r"Software\RegisteredApplications";
    const APPLICATION_NAME: &str = "Litematica Preview";
    const COMMAND_KEYS: [&str; 2] = [
        r"LitematicaPreview.Schematic\shell\open\command",
        r"Applications\LitematicaPreview.exe\shell\open\command",
    ];

    fn optional<T>(result: io::Result<T>) -> Result<Option<T>, String> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("Windows 文件关联：{error}")),
        }
    }

    fn value(root: &RegKey, path: &str, name: &str) -> Result<Option<String>, String> {
        let Some(key) = optional(root.open_subkey(path))? else {
            return Ok(None);
        };
        optional(key.get_value(name))
    }

    fn set(root: &RegKey, path: &str, name: &str, value: &str) -> Result<(), String> {
        let (key, _) = root
            .create_subkey(path)
            .map_err(|error| error.to_string())?;
        key.set_value(name, &value)
            .map_err(|error| error.to_string())
    }

    fn remove_value(root: &RegKey, path: &str, name: &str) -> Result<(), String> {
        if let Some(key) = optional(root.open_subkey_with_flags(path, KEY_READ | KEY_WRITE))? {
            optional(key.delete_value(name))?;
        }
        Ok(())
    }

    fn executable() -> Result<String, String> {
        std::env::current_exe()
            .map_err(|error| error.to_string())?
            .into_os_string()
            .into_string()
            .map_err(|_| "应用程序路径不是有效的 Unicode。".to_string())
    }

    fn is_other_command(command: Option<&str>, expected: &str) -> bool {
        command
            .is_some_and(|command| !command.is_empty() && !command.eq_ignore_ascii_case(expected))
    }

    fn notify_shell() {
        // SHCNE_ASSOCCHANGED requires no item list.
        unsafe {
            SHChangeNotify(
                SHCNE_ASSOCCHANGED as i32,
                SHCNF_IDLIST,
                ptr::null(),
                ptr::null(),
            )
        };
    }

    pub fn register(selected: &[&str]) -> Result<(), String> {
        let executable = executable()?;
        let command = format!("\"{executable}\" \"%1\"");
        let user = RegKey::predef(HKEY_CURRENT_USER);
        let classes = RegKey::predef(HKEY_CLASSES_ROOT);

        // Check per-user and merged class registrations before writing. The merged
        // view also includes machine-wide owners.
        for path in COMMAND_KEYS {
            let user_command = value(&user, &format!(r"Software\Classes\{path}"), "")?;
            let effective_command = value(&classes, path, "")?;
            if is_other_command(user_command.as_deref(), &command)
                || is_other_command(effective_command.as_deref(), &command)
            {
                return Err("另一份 Litematica Preview 已拥有文件关联。请先注销那份副本，再注册这一份。".to_string());
            }
        }
        if value(&user, REGISTERED_APPLICATIONS, APPLICATION_NAME)?
            .is_some_and(|path| !path.is_empty() && !path.eq_ignore_ascii_case(CAPABILITIES_KEY))
        {
            return Err(
                "另一个应用占用了 Litematica Preview 的注册项。".to_string(),
            );
        }

        let owns_registration = value(&user, &format!(r"{PROG_KEY}\shell\open\command"), "")?
            .is_some_and(|owner| owner.eq_ignore_ascii_case(&command));
        if owns_registration {
            for extension in crate::EXTENSIONS
                .iter()
                .filter(|extension| !selected.contains(extension))
            {
                let key = format!(r"Software\Classes\{extension}");
                if value(&user, &key, "")?.is_some_and(|default| default == PROG_ID) {
                    remove_value(&user, &key, "")?;
                }
                remove_value(&user, &format!(r"{key}\OpenWithProgids"), PROG_ID)?;
                remove_value(
                    &user,
                    &format!(r"{APPLICATION_KEY}\SupportedTypes"),
                    extension,
                )?;
                remove_value(
                    &user,
                    &format!(r"{CAPABILITIES_KEY}\FileAssociations"),
                    extension,
                )?;
            }
        }
        set(&user, PROG_KEY, "", "Minecraft 投影文件")?;
        set(
            &user,
            &format!(r"{PROG_KEY}\DefaultIcon"),
            "",
            &format!("\"{executable}\",0"),
        )?;
        set(
            &user,
            &format!(r"{PROG_KEY}\shell\open\command"),
            "",
            &command,
        )?;
        set(&user, APPLICATION_KEY, "FriendlyAppName", APPLICATION_NAME)?;
        set(
            &user,
            &format!(r"{APPLICATION_KEY}\shell\open\command"),
            "",
            &command,
        )?;
        set(&user, CAPABILITIES_KEY, "ApplicationName", APPLICATION_NAME)?;
        set(
            &user,
            CAPABILITIES_KEY,
            "ApplicationDescription",
            "离线查看 Minecraft 投影与结构。",
        )?;

        for extension in selected {
            let extension_key = format!(r"Software\Classes\{extension}");
            // Claim only unclaimed extensions. UserChoice is protected, and an
            // existing default belongs to its owner.
            if value(&classes, extension, "")?.is_none_or(|default| default.is_empty()) {
                set(&user, &extension_key, "", PROG_ID)?;
            }
            let (open_with, _) = user
                .create_subkey(format!(r"{extension_key}\OpenWithProgids"))
                .map_err(|error| error.to_string())?;
            open_with
                .set_raw_value(
                    PROG_ID,
                    &RegValue {
                        bytes: Cow::Borrowed(&[]),
                        vtype: REG_NONE,
                    },
                )
                .map_err(|error| error.to_string())?;
            set(
                &user,
                &format!(r"{APPLICATION_KEY}\SupportedTypes"),
                extension,
                "",
            )?;
            set(
                &user,
                &format!(r"{CAPABILITIES_KEY}\FileAssociations"),
                extension,
                PROG_ID,
            )?;
        }
        set(
            &user,
            REGISTERED_APPLICATIONS,
            APPLICATION_NAME,
            CAPABILITIES_KEY,
        )?;
        notify_shell();
        Ok(())
    }

    pub fn unregister() -> Result<(), String> {
        let expected = format!("\"{}\" \"%1\"", executable()?);
        let user = RegKey::predef(HKEY_CURRENT_USER);
        let owner = value(&user, &format!(r"{PROG_KEY}\shell\open\command"), "")?;
        if !owner.is_some_and(|owner| owner.eq_ignore_ascii_case(&expected)) {
            return Ok(());
        }
        let application_owner =
            value(&user, &format!(r"{APPLICATION_KEY}\shell\open\command"), "")?;
        if is_other_command(application_owner.as_deref(), &expected) {
            return Ok(());
        }
        let registered = value(&user, REGISTERED_APPLICATIONS, APPLICATION_NAME)?;
        if registered
            .as_deref()
            .is_some_and(|path| !path.is_empty() && !path.eq_ignore_ascii_case(CAPABILITIES_KEY))
        {
            return Ok(());
        }

        for extension in crate::EXTENSIONS {
            let extension_key = format!(r"Software\Classes\{extension}");
            if value(&user, &extension_key, "")?.is_some_and(|default| default == PROG_ID) {
                remove_value(&user, &extension_key, "")?;
            }
            remove_value(&user, &format!(r"{extension_key}\OpenWithProgids"), PROG_ID)?;
        }
        optional(user.delete_subkey_all(PROG_KEY))?;
        optional(user.delete_subkey_all(APPLICATION_KEY))?;
        // Remove registration capabilities without deleting unrelated user preferences.
        optional(user.delete_subkey_all(CAPABILITIES_KEY))?;
        remove_value(&user, REGISTERED_APPLICATIONS, APPLICATION_NAME)?;
        notify_shell();
        Ok(())
    }

    pub fn open_settings() -> Result<(), String> {
        // Use the general Default Apps page available on Windows 10 and Windows 11.
        let operation: Vec<u16> = "open\0".encode_utf16().collect();
        let uri: Vec<u16> = "ms-settings:defaultapps\0".encode_utf16().collect();
        let result = unsafe {
            ShellExecuteW(
                ptr::null_mut(),
                operation.as_ptr(),
                uri.as_ptr(),
                ptr::null(),
                ptr::null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        if result <= 32 {
            return Err(format!(
                "无法打开 Windows 默认应用设置（错误 {result}）。"
            ));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn register(_: &[&str]) -> Result<(), String> {
        Err("仅在 Windows 上支持文件关联。".to_string())
    }

    pub fn unregister() -> Result<(), String> {
        Err("仅在 Windows 上支持文件关联。".to_string())
    }

    pub fn open_settings() -> Result<(), String> {
        Err("仅在 Windows 上支持“默认应用”设置。".to_string())
    }
}

pub use platform::{open_settings, unregister};
