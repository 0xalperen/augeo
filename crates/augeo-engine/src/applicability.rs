//! Decides whether a tweak applies to this machine, and says why not.

use augeo_catalog::{
    lint::parse_build_rule,
    schema::{Applicability, ChassisRule, CpuLayoutRule, CpuVendorRule, GpuRule},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuVendor {
    Amd,
    Intel,
    Other,
}

/// Facts about this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    pub build: u32,
    pub nvidia: bool,
    pub laptop: bool,
    pub cpu_vendor: CpuVendor,
    pub dual_ccd_x3d: bool,
    /// G-SYNC or another VRR mode is active on the primary display.
    pub vrr_active: bool,
}

/// `Ok` when the tweak applies, otherwise the reason it doesn't, written for the user.
pub fn check(rule: &Applicability, env: &Environment) -> Result<(), String> {
    match parse_build_rule(&rule.build) {
        Some(minimum) if env.build < minimum => {
            return Err(format!(
                "Needs Windows build {minimum} or newer; this PC has {}.",
                env.build
            ));
        }
        Some(_) => {}
        None => return Err(format!("Invalid build rule: {}", rule.build)),
    }
    if rule.gpu == GpuRule::Nvidia && !env.nvidia {
        return Err("Needs an NVIDIA GPU with its driver installed.".to_string());
    }
    match rule.chassis {
        ChassisRule::Desktop if env.laptop => {
            return Err("Desktop only: it would cost battery life on a laptop.".to_string());
        }
        ChassisRule::Laptop if !env.laptop => return Err("Laptops only.".to_string()),
        _ => {}
    }
    match rule.cpu_vendor {
        CpuVendorRule::Amd if env.cpu_vendor != CpuVendor::Amd => {
            return Err("AMD CPUs only.".to_string());
        }
        CpuVendorRule::Intel if env.cpu_vendor != CpuVendor::Intel => {
            return Err("Intel CPUs only.".to_string());
        }
        _ => {}
    }
    if rule.vrr && !env.vrr_active {
        return Err(
            "Only when G-SYNC is active on your main display, because it adds latency otherwise."
                .to_string(),
        );
    }
    match rule.cpu_layout {
        CpuLayoutRule::DualCcdX3d if !env.dual_ccd_x3d => {
            Err("Only for AMD X3D CPUs with two core dies.".to_string())
        }
        CpuLayoutRule::NotDualCcdX3d if env.dual_ccd_x3d => Err(
            "Not for this CPU: AMD X3D chips with two core dies rely on core parking.".to_string(),
        ),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Environment {
        Environment {
            build: 26100,
            nvidia: true,
            laptop: false,
            cpu_vendor: CpuVendor::Amd,
            dual_ccd_x3d: false,
            vrr_active: false,
        }
    }

    fn rule() -> Applicability {
        Applicability::default()
    }

    #[test]
    fn the_default_rule_applies_everywhere_supported() {
        assert_eq!(check(&rule(), &env()), Ok(()));
        let old = Environment {
            build: 18363,
            ..env()
        };
        assert!(check(&rule(), &old).unwrap_err().contains("build 19041"));
    }

    #[test]
    fn build_rules() {
        let win11 = Applicability {
            build: ">=22000".into(),
            ..rule()
        };
        assert_eq!(check(&win11, &env()), Ok(()));
        let win10 = Environment {
            build: 19045,
            ..env()
        };
        assert!(check(&win11, &win10).unwrap_err().contains("22000"));
        let exact = Environment {
            build: 22000,
            ..env()
        };
        assert_eq!(check(&win11, &exact), Ok(()));
    }

    #[test]
    fn gpu_rules() {
        let nvidia = Applicability {
            gpu: GpuRule::Nvidia,
            ..rule()
        };
        assert_eq!(check(&nvidia, &env()), Ok(()));
        let none = Environment {
            nvidia: false,
            ..env()
        };
        assert!(check(&nvidia, &none).unwrap_err().contains("NVIDIA"));
    }

    #[test]
    fn chassis_rules() {
        let desktop = Applicability {
            chassis: ChassisRule::Desktop,
            ..rule()
        };
        let laptop_env = Environment {
            laptop: true,
            ..env()
        };
        assert_eq!(check(&desktop, &env()), Ok(()));
        assert!(
            check(&desktop, &laptop_env)
                .unwrap_err()
                .contains("battery")
        );
        let laptop = Applicability {
            chassis: ChassisRule::Laptop,
            ..rule()
        };
        assert_eq!(check(&laptop, &laptop_env), Ok(()));
        assert!(check(&laptop, &env()).is_err());
    }

    #[test]
    fn vendor_rules() {
        let amd = Applicability {
            cpu_vendor: CpuVendorRule::Amd,
            ..rule()
        };
        let intel = Applicability {
            cpu_vendor: CpuVendorRule::Intel,
            ..rule()
        };
        assert_eq!(check(&amd, &env()), Ok(()));
        assert!(check(&intel, &env()).is_err());
        let intel_env = Environment {
            cpu_vendor: CpuVendor::Intel,
            ..env()
        };
        assert_eq!(check(&intel, &intel_env), Ok(()));
        assert!(check(&amd, &intel_env).is_err());
    }

    #[test]
    fn vrr_rules() {
        let needs_vrr = Applicability {
            vrr: true,
            ..rule()
        };
        assert!(check(&needs_vrr, &env()).unwrap_err().contains("G-SYNC"));
        let active = Environment {
            vrr_active: true,
            ..env()
        };
        assert_eq!(check(&needs_vrr, &active), Ok(()));
    }

    #[test]
    fn x3d_rules() {
        let only = Applicability {
            cpu_layout: CpuLayoutRule::DualCcdX3d,
            ..rule()
        };
        let never = Applicability {
            cpu_layout: CpuLayoutRule::NotDualCcdX3d,
            ..rule()
        };
        let x3d = Environment {
            dual_ccd_x3d: true,
            ..env()
        };
        assert_eq!(check(&only, &x3d), Ok(()));
        assert!(check(&only, &env()).is_err());
        assert_eq!(check(&never, &env()), Ok(()));
        assert!(check(&never, &x3d).unwrap_err().contains("core parking"));
    }
}
