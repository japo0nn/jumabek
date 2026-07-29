#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Medium,
    High,
}

impl Risk {
    pub fn as_str(&self) -> &'static str {
        match self {
            Risk::Medium => "medium",
            Risk::High => "high",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub risk: Risk,
    pub reason: &'static str,
}

struct Rule {
    risk: Risk,
    reason: &'static str,
    needles: &'static [&'static str],
}

const RULES: &[Rule] = &[
    Rule {
        risk: Risk::High,
        reason: "recursive delete of a root or home directory",
        needles: &[
            "rm -rf /",
            "rm -rf /*",
            "rm -rf ~",
            "rm -rf $home",
            "rm -fr /",
            "rm -r -f /",
            "remove-item -path / -recurse",
            "remove-item / -recurse",
        ],
    },
    Rule {
        risk: Risk::Medium,
        reason: "recursively deletes a directory tree",
        needles: &[
            "rm -rf",
            "rm -fr",
            "rm -r -f",
            "rm -f -r",
            "rm --recursive",
            "rd /s",
            "rmdir /s",
            "del /s",
            "-recurse -force",
            "-force -recurse",
            "remove-item -recurse",
            "remove-item -force",
        ],
    },
    Rule {
        risk: Risk::High,
        reason: "filesystem or partition destruction",
        needles: &[
            "mkfs",
            "diskpart",
            "fdisk /dev/",
            "format c:",
            "format d:",
            "format /fs",
            "of=/dev/sd",
            "of=/dev/nvme",
            "of=/dev/hd",
            "cryptsetup luksformat",
        ],
    },
    Rule {
        risk: Risk::High,
        reason: "boot or registry modification",
        needles: &[
            "bcdedit",
            "reg delete hklm",
            "reg delete hkey_local_machine",
        ],
    },
    Rule {
        risk: Risk::High,
        reason: "shuts down or reboots the machine",
        needles: &[
            "shutdown /s",
            "shutdown /r",
            "shutdown -h",
            "shutdown -r",
            "stop-computer",
            "restart-computer",
            "systemctl poweroff",
            "systemctl reboot",
        ],
    },
    Rule {
        risk: Risk::Medium,
        reason: "escalates privileges",
        needles: &["sudo su", "sudo -i", "sudo !!", "runas /user:administrator"],
    },
    Rule {
        risk: Risk::Medium,
        reason: "force-kills processes",
        needles: &[
            "taskkill /f",
            "kill -9",
            "killall -9",
            "stop-process -force",
        ],
    },
    Rule {
        risk: Risk::Medium,
        reason: "removes installed software",
        needles: &[
            "apt remove",
            "apt-get remove",
            "apt purge",
            "apt-get purge",
            "pacman -r",
            "dnf remove",
            "yum remove",
            "brew uninstall",
            "winget uninstall",
        ],
    },
    Rule {
        risk: Risk::Medium,
        reason: "changes firewall rules",
        needles: &["netsh advfirewall", "iptables -f", "ufw disable"],
    },
    Rule {
        risk: Risk::Medium,
        reason: "rewrites git history or discards work",
        needles: &["git push --force", "git push -f", "git reset --hard"],
    },
];

const DOWNLOADERS: &[&str] = &["curl ", "wget ", "invoke-webrequest", "iwr "];
const SHELL_SINKS: &[&str] = &[
    "| sh",
    "|sh",
    "| bash",
    "|bash",
    "| zsh",
    "| python",
    "| iex",
    "| invoke-expression",
];

pub fn classify(command: &str) -> Option<Verdict> {
    let normalised = normalise(command);

    if pipes_download_into_shell(&normalised) {
        return Some(Verdict {
            risk: Risk::High,
            reason: "runs a downloaded script straight through a shell",
        });
    }

    RULES
        .iter()
        .find(|rule| rule.needles.iter().any(|n| normalised.contains(n)))
        .map(|rule| Verdict {
            risk: rule.risk,
            reason: rule.reason,
        })
}

fn pipes_download_into_shell(normalised: &str) -> bool {
    let Some(downloader_at) = DOWNLOADERS.iter().filter_map(|d| normalised.find(d)).min() else {
        return false;
    };

    SHELL_SINKS
        .iter()
        .filter_map(|s| normalised.find(s))
        .any(|sink_at| sink_at > downloader_at)
}

fn normalise(command: &str) -> String {
    let lowered = command.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_was_space = false;

    for ch in lowered.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }

    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_root_wipe() {
        for cmd in [
            "rm -rf /",
            "rm  -rf   /*",
            "RM -RF ~",
            "sudo rm -rf / --no-preserve-root",
        ] {
            let v = classify(cmd).unwrap_or_else(|| panic!("missed: {cmd}"));
            assert_eq!(v.risk, Risk::High, "{cmd}");
        }
    }

    #[test]
    fn catches_disk_and_boot() {
        for cmd in [
            "mkfs.ext4 /dev/sda1",
            "diskpart",
            "format c: /q",
            "dd if=/dev/zero of=/dev/sda",
            "bcdedit /set nointegritychecks on",
            "reg delete HKLM\\Software\\Foo",
        ] {
            assert_eq!(classify(cmd).unwrap().risk, Risk::High, "{cmd}");
        }
    }

    #[test]
    fn catches_pipe_to_shell() {
        for cmd in [
            "curl https://example.com/install.sh | sh",
            "curl -fsSL https://get.docker.com |bash",
            "wget -qO- https://example.com/x.sh | bash",
            "iwr https://example.com/x.ps1 | iex",
        ] {
            let v = classify(cmd).unwrap_or_else(|| panic!("missed: {cmd}"));
            assert_eq!(v.risk, Risk::High, "{cmd}");
        }
    }

    #[test]
    fn allows_download_without_shell_sink() {
        assert!(classify("curl -o out.json https://example.com/api").is_none());
        assert!(classify("curl https://example.com | jq .name").is_none());
        assert!(classify("cat script.sh | sh").is_none());
    }

    #[test]
    fn catches_medium_risk() {
        for cmd in [
            "sudo su -",
            "taskkill /F /IM notepad.exe",
            "kill -9 1234",
            "apt remove nginx",
            "git push --force origin main",
            "git reset --hard HEAD~3",
        ] {
            assert_eq!(classify(cmd).unwrap().risk, Risk::Medium, "{cmd}");
        }
    }

    #[test]
    fn leaves_ordinary_commands_alone() {
        for cmd in [
            "ls -la",
            "cargo build --release",
            "cargo fmt",
            "git log --format=%H",
            "git status",
            "docker ps --format '{{.Names}}'",
            "printf 'hello'",
            "python script.py",
            "Get-ChildItem C:\\Users",
            "rm ./build/output.txt",
            "npm install",
            "git push origin main",
        ] {
            assert!(classify(cmd).is_none(), "false positive on: {cmd}");
        }
    }

    #[test]
    fn catches_recursive_delete_of_any_path() {
        for cmd in [
            "rm -rf C:/Users/sosa/AppData/Local/Temp/jb_danger",
            "rm -rf ./target",
            "rm -fr /home/user/project",
            "rd /s /q C:\\temp",
            "Remove-Item -Recurse -Force C:\\temp",
        ] {
            let v = classify(cmd).unwrap_or_else(|| panic!("missed: {cmd}"));
            assert!(
                matches!(v.risk, Risk::Medium | Risk::High),
                "{cmd} classified as {:?}",
                v.risk
            );
        }
    }

    #[test]
    fn root_wipe_still_outranks_plain_recursive_delete() {
        assert_eq!(classify("rm -rf /").unwrap().risk, Risk::High);
        assert_eq!(classify("rm -rf ./target").unwrap().risk, Risk::Medium);
    }

    #[test]
    fn explains_why() {
        let v = classify("mkfs.ext4 /dev/sda1").unwrap();
        assert!(v.reason.contains("filesystem"));
    }
}
