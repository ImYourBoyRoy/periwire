//! `periwire driver ...`: the Windows virtual USB host controller driver (other systems have nothing to install).
use crate::args::DriverCmd;
use anyhow::Result;

#[cfg(not(windows))]
pub(crate) fn driver_cmd(_: DriverCmd, _json: bool) -> Result<i32> {
    println!("Only Windows needs a separate driver. Linux uses the kernel's vhci-hcd module and macOS uses Apple's IOUSBHostControllerInterface.");
    Ok(0)
}

#[cfg(windows)]
pub(crate) fn driver_cmd(dc: DriverCmd, json: bool) -> Result<i32> {
    use crate::windrv;
    let print_status = |st: &windrv::Status| {
        if json {
            cli::print_json(&serde_json::to_value(st).unwrap_or_default());
            return;
        }
        println!("{} {}", ui::bold("Windows virtual USB host controller"), format!("(usbip-win2 {}, carried inside this program)", st.embedded_version));
        let pk = if st.packages.is_empty() {
            "not installed".to_string()
        } else {
            st.packages.iter().map(|p| format!("{} ({}, {})", p.original.trim_end_matches(".inf"), p.published, p.version)).collect::<Vec<_>>().join(", ")
        };
        println!("  driver packages  {pk}");
        println!("  host controller  {}", st.device.clone().unwrap_or_else(|| "not present".into()));
        println!("  tools            {}", st.tools.clone().unwrap_or_else(|| "not extracted yet".into()));
        println!("  C++ runtime      {}", if st.vc_runtime { "ok" } else { "missing: install the Microsoft Visual C++ 2015-2022 x64 runtime (winget install Microsoft.VCRedist.2015+.x64)" });
        println!("  this shell       {}", if st.admin { "elevated (Administrator)" } else { "not elevated" });
        println!("  ready to import  {}", if st.ready { "yes" } else { "no: run `periwire driver install` as Administrator" });
    };
    match dc {
        DriverCmd::Status => {
            let st = windrv::status();
            print_status(&st);
            Ok(if st.ready { 0 } else { 1 })
        }
        DriverCmd::Install { elevate } => {
            if !windrv::is_admin() && elevate {
                eprintln!("asking Windows for administrator rights...");
                let code = windrv::elevate_and_wait(&["driver", "install"])?;
                let st = windrv::status();
                print_status(&st);
                return Ok(if st.ready {
                    0
                } else if code != 0 {
                    code
                } else {
                    1
                });
            }
            let reboot = windrv::install()?;
            print_status(&windrv::status());
            if reboot {
                println!("Windows asked for a restart to finish installing the driver.");
            }
            Ok(0)
        }
        DriverCmd::Uninstall { elevate } => {
            if !windrv::is_admin() && elevate {
                let code = windrv::elevate_and_wait(&["driver", "uninstall"])?;
                print_status(&windrv::status());
                return Ok(code);
            }
            windrv::uninstall()?;
            println!("removed the virtual host controller, both driver packages and the extracted tools");
            Ok(0)
        }
        DriverCmd::Extract { dir } => {
            windrv::extract(&dir)?;
            println!("wrote the usbip-win2 {} driver packages and tools to {}", windrv::VERSION, dir.display());
            println!("install by hand with: pnputil /add-driver usbip2_filter.inf /install  then  devnode.exe install usbip2_ude.inf {}", windrv::HWID);
            Ok(0)
        }
    }
}
