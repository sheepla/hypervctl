//! VHD attachment and file information, plus the path comparison used to match
//! attachments to virtual disks.

use std::path::PathBuf;

use crate::ids::VmId;

/// A virtual disk attached to a VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VhdAttachment {
    pub vm_id: VmId,
    pub vm_name: String,
    pub controller: Controller,
    pub path: PathBuf,
}

/// Controller slot an attachment sits on. `SCSI 0:1` is `{ scsi, 0, 1 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Controller {
    pub kind: ControllerKind,
    pub number: u8,
    pub lun: u8,
}

/// Controller type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControllerKind {
    Ide,
    Scsi,
    Sata,
    /// Value the backend did not map; kept so it can be shown instead of dropped.
    Unknown(u16),
}

/// File information for a virtual disk, attached or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VhdInfo {
    pub path: PathBuf,
    pub format: VhdFormat,
    pub kind: VhdKind,
    pub virtual_size_bytes: u64,
    pub file_size_bytes: u64,
    pub block_size_bytes: Option<u64>,
    pub parent_path: Option<PathBuf>,
    pub disk_id: Option<String>,
}

/// An attachment together with the file behind it, as `vhd list` reports them.
/// The columns about the slot come from the attachment; the ones about the
/// file (format, sizes) stay unknown when the file is gone or unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedVhd {
    pub attachment: VhdAttachment,
    pub info: Option<VhdInfo>,
}

/// Container format of a virtual disk file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VhdFormat {
    Vhd,
    Vhdx,
    /// Value the backend did not map; kept so it can be shown instead of dropped.
    Unknown(u16),
}

/// Allocation type of a virtual disk file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VhdKind {
    Fixed,
    Dynamic,
    Differencing,
    /// Value the backend did not map; kept so it can be shown instead of dropped.
    Unknown(u16),
}

/// Builds a canonical key for comparing two VHD paths.
///
/// Windows reports the same file in several spellings: the `\\?\` device prefix,
/// `\\?\UNC\` share paths, a forward slash here and there, and any mix of upper
/// and lower case. The key is **not** meant for display.
pub fn normalize_vhd_path(path: &str) -> String {
    let trimmed = path.trim().trim_matches('"');
    let unextended = if let Some(rest) = trimmed.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = trimmed.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        trimmed.to_string()
    };
    // Windows treats paths case-insensitively and accepts both separators.
    let unified = unextended.replace('/', r"\").to_ascii_lowercase();
    // A trailing separator never changes which file a path points at.
    let untailed = unified.trim_end_matches('\\');
    if untailed.is_empty() {
        unified
    } else {
        untailed.to_string()
    }
}

/// `true` when both paths name the same file on Windows.
pub fn vhd_path_eq(left: &str, right: &str) -> bool {
    normalize_vhd_path(left) == normalize_vhd_path(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn compares_paths_ignoring_case_and_device_prefix() {
        assert!(vhd_path_eq(r"\\?\C:\VHDs\Disk.vhdx", r"c:\vhds\DISK.VHDX"));
        assert!(!vhd_path_eq(r"C:\VHDs\Disk.vhdx", r"C:\VHDs\Other.vhdx"));
    }

    #[test]
    fn compares_share_paths_with_and_without_the_unc_prefix() {
        assert!(vhd_path_eq(
            r"\\?\UNC\FILESERV\vhds\disk.vhdx",
            r"\\fileserv\VHDS\disk.vhdx"
        ));
    }

    #[test]
    fn compares_paths_with_either_separator_and_trailing_separator() {
        assert!(vhd_path_eq(r"C:/vhds/disk.vhdx", r"C:\VHDS\disk.vhdx"));
        assert!(vhd_path_eq(r"C:\vhds", r"C:\vhds\"));
    }

    #[test]
    fn normalization_is_idempotent_and_keeps_the_root() {
        let normalized = normalize_vhd_path(r"\\?\C:\VHDs\Disk.vhdx");
        assert_eq!(normalize_vhd_path(&normalized), normalized);
        assert_eq!(normalize_vhd_path(r"C:\"), "c:");
        assert_eq!(normalize_vhd_path(""), "");
    }

    #[test]
    fn attachment_carries_its_controller_slot() {
        let attachment = VhdAttachment {
            vm_id: VmId::new(Uuid::nil()),
            vm_name: "test".to_string(),
            controller: Controller {
                kind: ControllerKind::Scsi,
                number: 0,
                lun: 1,
            },
            path: PathBuf::from(r"C:\vhds\disk.vhdx"),
        };
        assert_eq!(attachment.controller.number, 0);
        assert_eq!(attachment.controller.lun, 1);
        assert_eq!(
            normalize_vhd_path(attachment.path.to_str().unwrap_or_default()),
            "c:\\vhds\\disk.vhdx"
        );
    }

    #[test]
    fn unknown_values_keep_their_raw_value() {
        assert_eq!(VhdFormat::Unknown(9), VhdFormat::Unknown(9));
        assert_eq!(VhdKind::Unknown(9), VhdKind::Unknown(9));
        assert_eq!(ControllerKind::Unknown(9), ControllerKind::Unknown(9));
    }
}
