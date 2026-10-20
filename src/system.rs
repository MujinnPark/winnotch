//! System watchers: volume, brightness, Caps Lock, keyboard language, charger,
//! Wi-Fi and audio-output (headphones) changes. They run on background threads and
//! report `SysEvent`s that the notch shows as small toasts.
//! The first reading of each value is only a baseline, so nothing pops up at start-up.

use std::sync::mpsc;

#[derive(Clone, Debug)]
pub enum SysEvent {
    /// level 0..1
    Volume {
        level: f32,
        muted: bool,
    },
    /// level 0..1
    Brightness(f32),
    CapsLock(bool),
    Lang(String),
    Power {
        plugged: bool,
        percent: u8,
    },
    Wifi {
        connected: bool,
        name: String,
    },
    /// The default audio output changed (headphones, speakers, ...).
    Audio(String),
}

pub fn spawn() -> mpsc::Receiver<SysEvent> {
    let (tx, rx) = mpsc::channel();
    imp::start(tx);
    rx
}

/// Short code for the primary language of a Windows LANGID.
#[allow(dead_code)]
fn lang_name(langid: u16) -> String {
    match langid & 0x3FF {
        0x09 => "English".into(),
        0x3E => "Malay".into(),
        0x21 => "Indonesian".into(),
        0x04 => "Chinese".into(),
        0x11 => "Japanese".into(),
        0x12 => "Korean".into(),
        0x0C => "French".into(),
        0x07 => "German".into(),
        0x0A => "Spanish".into(),
        0x16 => "Portuguese".into(),
        0x19 => "Russian".into(),
        0x01 => "Arabic".into(),
        0x39 => "Hindi".into(),
        0x1E => "Thai".into(),
        0x2A => "Vietnamese".into(),
        0x49 => "Tamil".into(),
        other => format!("Language {:#04x}", other),
    }
}

#[cfg(not(windows))]
mod imp {
    use super::SysEvent;
    use std::sync::mpsc::Sender;

    pub fn start(_tx: Sender<SysEvent>) {}
}

#[cfg(windows)]
mod imp {
    use super::{lang_name, SysEvent};
    use std::io::{BufRead, BufReader};
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::mpsc::Sender;
    use std::thread;
    use std::time::Duration;

    use windows::Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation};
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, GetKeyboardLayout, VK_CAPITAL};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    pub fn start(tx: Sender<SysEvent>) {
        let tx2 = tx.clone();
        thread::spawn(move || poll_loop(tx));
        thread::spawn(move || brightness_loop(tx2));
    }

    /// One long-lived hidden PowerShell prints the laptop panel brightness (0-100)
    /// whenever it is read; desktop PCs / external monitors simply print nothing.
    fn brightness_loop(tx: Sender<SysEvent>) {
        let script = "$ErrorActionPreference='Stop'; while($true){ $b=$null; \
            try { $b=(Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightness | \
            Select-Object -First 1).CurrentBrightness } catch {}; \
            if($b -ne $null){ [Console]::Out.WriteLine($b); [Console]::Out.Flush() }; \
            Start-Sleep -Milliseconds 600 }";
        let mut cmd = Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let Ok(mut child) = cmd.spawn() else {
            return;
        };
        let Some(out) = child.stdout.take() else {
            return;
        };
        let mut last: Option<f32> = None;
        for line in BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            let Ok(v) = line.trim().parse::<f32>() else {
                continue;
            };
            let v = (v / 100.0).clamp(0.0, 1.0);
            if let Some(l) = last {
                if (l - v).abs() > 0.004 && tx.send(SysEvent::Brightness(v)).is_err() {
                    break;
                }
            }
            last = Some(v);
        }
        let _ = child.kill();
    }

    struct AudioReading {
        dev: IMMDevice,
        id: String,
        level: f32,
        muted: bool,
    }

    /// Describes the new default output (the id is only a stable identifier).
    fn friendly_name(_dev: &IMMDevice) -> String {
        "Output device changed".to_string()
    }

    unsafe fn read_audio(en: &IMMDeviceEnumerator) -> Option<AudioReading> {
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let id_ptr = dev.GetId().ok()?;
        let id = id_ptr.to_string().ok()?;
        CoTaskMemFree(Some(id_ptr.0 as *const std::ffi::c_void));
        let vol: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None).ok()?;
        let level = vol.GetMasterVolumeLevelScalar().ok()?;
        let muted = vol.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        Some(AudioReading {
            dev,
            id,
            level,
            muted,
        })
    }

    fn wifi_state() -> (bool, String) {
        match NetworkInformation::GetInternetConnectionProfile() {
            Ok(p) => {
                let online = p.GetNetworkConnectivityLevel().ok()
                    == Some(NetworkConnectivityLevel::InternetAccess);
                let name = if p.IsWlanConnectionProfile().unwrap_or(false) {
                    p.ProfileName().map(|s| s.to_string()).unwrap_or_default()
                } else {
                    String::new()
                };
                (online, name)
            }
            Err(_) => (false, String::new()),
        }
    }

    fn poll_loop(tx: Sender<SysEvent>) {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let enumerator: Option<IMMDeviceEnumerator> =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() };

        let mut last_dev: Option<String> = None;
        let mut last_vol: Option<(f32, bool)> = None;
        let mut last_caps: Option<bool> = None;
        let mut last_lang: Option<u16> = None;
        let mut last_plugged: Option<bool> = None;
        let mut last_wifi: Option<(bool, String)> = None;
        let mut tick: u32 = 0;

        loop {
            let mut events: Vec<SysEvent> = Vec::new();

            // ---- volume / default output device ----
            if let Some(en) = &enumerator {
                if let Some(a) = unsafe { read_audio(en) } {
                    if last_dev.as_deref() != Some(a.id.as_str()) {
                        if last_dev.is_some() {
                            events.push(SysEvent::Audio(friendly_name(&a.dev)));
                        }
                        last_dev = Some(a.id.clone());
                        last_vol = Some((a.level, a.muted));
                    } else if let Some((l, m)) = last_vol {
                        if (l - a.level).abs() > 0.004 || m != a.muted {
                            events.push(SysEvent::Volume {
                                level: a.level,
                                muted: a.muted,
                            });
                            last_vol = Some((a.level, a.muted));
                        }
                    }
                }
            }

            // ---- caps lock ----
            let caps = unsafe { (GetKeyState(VK_CAPITAL.0 as i32) & 1) != 0 };
            if let Some(c) = last_caps {
                if c != caps {
                    events.push(SysEvent::CapsLock(caps));
                }
            }
            last_caps = Some(caps);

            // ---- keyboard language (of the focused window) ----
            let langid = unsafe {
                let fg = GetForegroundWindow();
                let tid = GetWindowThreadProcessId(fg, None);
                (GetKeyboardLayout(tid).0 as usize & 0xFFFF) as u16
            };
            if let Some(l) = last_lang {
                if l != langid {
                    events.push(SysEvent::Lang(lang_name(langid)));
                }
            }
            last_lang = Some(langid);

            // ---- charger ----
            let mut ps = SYSTEM_POWER_STATUS::default();
            if unsafe { GetSystemPowerStatus(&mut ps) }.is_ok() && ps.BatteryFlag & 128 == 0 {
                let plugged = ps.ACLineStatus == 1;
                if let Some(p) = last_plugged {
                    if p != plugged {
                        events.push(SysEvent::Power {
                            plugged,
                            percent: ps.BatteryLifePercent.min(100),
                        });
                    }
                }
                last_plugged = Some(plugged);
            }

            // ---- Wi-Fi / internet (every ~2 s) ----
            if tick % 16 == 0 {
                let now = wifi_state();
                if let Some(prev) = &last_wifi {
                    if *prev != now {
                        events.push(SysEvent::Wifi {
                            connected: now.0,
                            name: now.1.clone(),
                        });
                    }
                }
                last_wifi = Some(now);
            }

            for e in events {
                if tx.send(e).is_err() {
                    return;
                }
            }
            tick = tick.wrapping_add(1);
            thread::sleep(Duration::from_millis(125));
        }
    }
}
