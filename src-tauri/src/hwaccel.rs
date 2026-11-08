//! 硬件视频编码器检测与参数映射。
//!
//! 启动时通过 `ffmpeg -encoders` 找到候选硬件编码器，再实际打开一次编码器确认
//! 当前机器可用（NVENC / QSV / AMF）；转码时根据用户选择和探测结果将软件编码器
//! 参数映射为硬件编码器参数。

use serde::{Deserialize, Serialize};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 用户在设置中选择的编码器偏好
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncoderPreference {
    /// 自动检测（优先级：NVENC > QSV > AMF > 软编）
    Auto,
    /// 强制使用 NVIDIA NVENC（不可用时回退软编）
    Nvenc,
    /// 强制使用 Intel QSV（不可用时回退软编）
    Qsv,
    /// 强制使用 AMD AMF（不可用时回退软编）
    Amf,
    /// 强制使用软件编码
    Software,
}

impl Default for EncoderPreference {
    fn default() -> Self {
        Self::Auto
    }
}

impl EncoderPreference {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Nvenc => "nvenc",
            Self::Qsv => "qsv",
            Self::Amf => "amf",
            Self::Software => "software",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "nvenc" => Self::Nvenc,
            "qsv" => Self::Qsv,
            "amf" => Self::Amf,
            "software" => Self::Software,
            _ => Self::Auto,
        }
    }
}

/// 硬件编码器类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwEncoder {
    Nvenc,
    Qsv,
    Amf,
}

impl HwEncoder {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Nvenc => "NVENC",
            Self::Qsv => "QSV",
            Self::Amf => "AMF",
        }
    }
}

/// 硬件编码器探测结果
#[derive(Debug, Clone, Default)]
pub struct HwAccelCapabilities {
    /// 可用的 H.264 编码器（h264_nvenc / h264_qsv / h264_amf）
    pub h264: Vec<String>,
    /// 可用的 H.265 编码器（hevc_nvenc / hevc_qsv / hevc_amf）
    pub h265: Vec<String>,
    /// 可用的 AV1 编码器（av1_nvenc / av1_qsv / av1_amf）
    pub av1: Vec<String>,
}

fn probe_encoder(ffmpeg_path: &str, encoder: &str) -> bool {
    let mut command = Command::new(ffmpeg_path);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=1280x720:d=0.04",
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            encoder,
            "-f",
            "null",
            "-",
        ])
        .output()
        .map(|output| {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::debug!(
                    encoder,
                    status = ?output.status,
                    error = %stderr.trim(),
                    "硬件编码器单帧探测失败"
                );
            }
            output.status.success()
        })
        .unwrap_or_else(|error| {
            tracing::debug!(encoder, %error, "硬件编码器单帧探测进程启动失败");
            false
        })
}

impl HwAccelCapabilities {
    /// 检测所有可用的硬件编码器。
    ///
    /// `ffmpeg -encoders` 只表示当前 FFmpeg 构建包含该编码器，并不表示当前 GPU
    /// 支持它。对每个候选编码器实际执行一次单帧编码，避免把不支持的 AMF/NVENC
    /// 编码器误判为可用。
    pub fn detect(ffmpeg_path: &str) -> anyhow::Result<Self> {
        let mut command = Command::new(ffmpeg_path);
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        let output = command
            .args(["-encoders", "-hide_banner"])
            .output()?;

        if !output.status.success() {
            anyhow::bail!("ffmpeg -encoders 执行失败: {:?}", output.status);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut candidates = Vec::new();
        for line in stdout.lines() {
            let parts: Vec<&str> = line.trim().split_whitespace().collect();
            if parts.len() < 2 {
                continue;
            }
            let encoder_name = parts[1];
            if (encoder_name.contains("h264")
                || encoder_name.contains("hevc")
                || encoder_name.contains("h265")
                || encoder_name.contains("av1"))
                && (encoder_name.contains("nvenc")
                    || encoder_name.contains("qsv")
                    || encoder_name.contains("amf"))
            {
                candidates.push(encoder_name.to_string());
            }
        }

        let mut caps = Self::default();
        for encoder in candidates {
            if probe_encoder(ffmpeg_path, &encoder) {
                if encoder.contains("h264") {
                    caps.h264.push(encoder);
                } else if encoder.contains("hevc") || encoder.contains("h265") {
                    caps.h265.push(encoder);
                } else if encoder.contains("av1") {
                    caps.av1.push(encoder);
                }
            }
        }

        tracing::info!("硬件编码器探测完成:");
        tracing::info!("  H.264: {:?}", caps.h264);
        tracing::info!("  H.265: {:?}", caps.h265);
        tracing::info!("  AV1:   {:?}", caps.av1);

        Ok(caps)
    }

    /// 根据用户偏好和探测结果，选择最佳的 H.264 编码器
    pub fn choose_h264(&self, pref: EncoderPreference) -> Option<(HwEncoder, String)> {
        self.choose_encoder(&self.h264, pref)
    }

    /// 根据用户偏好和探测结果，选择最佳的 H.265 编码器
    pub fn choose_h265(&self, pref: EncoderPreference) -> Option<(HwEncoder, String)> {
        self.choose_encoder(&self.h265, pref)
    }

    /// 根据用户偏好和探测结果，选择最佳的 AV1 编码器
    pub fn choose_av1(&self, pref: EncoderPreference) -> Option<(HwEncoder, String)> {
        self.choose_encoder(&self.av1, pref)
    }

    fn choose_encoder(&self, available: &[String], pref: EncoderPreference) -> Option<(HwEncoder, String)> {
        match pref {
            EncoderPreference::Software => None,
            EncoderPreference::Nvenc => available
                .iter()
                .find(|e| e.contains("nvenc"))
                .map(|e| (HwEncoder::Nvenc, e.clone())),
            EncoderPreference::Qsv => available
                .iter()
                .find(|e| e.contains("qsv"))
                .map(|e| (HwEncoder::Qsv, e.clone())),
            EncoderPreference::Amf => available
                .iter()
                .find(|e| e.contains("amf"))
                .map(|e| (HwEncoder::Amf, e.clone())),
            EncoderPreference::Auto => {
                // 优先级: NVENC > QSV > AMF
                if let Some(e) = available.iter().find(|e| e.contains("nvenc")) {
                    Some((HwEncoder::Nvenc, e.clone()))
                } else if let Some(e) = available.iter().find(|e| e.contains("qsv")) {
                    Some((HwEncoder::Qsv, e.clone()))
                } else if let Some(e) = available.iter().find(|e| e.contains("amf")) {
                    Some((HwEncoder::Amf, e.clone()))
                } else {
                    None
                }
            }
        }
    }
}

/// 硬件编码器参数映射器
pub struct HwEncoderMapper {
    hw_type: HwEncoder,
}

impl HwEncoderMapper {
    pub fn new(hw_type: HwEncoder) -> Self {
        Self { hw_type }
    }

    /// 映射软件编码器的 preset（ultrafast ~ veryslow）到硬件编码器的 preset
    pub fn map_preset(&self, software_preset: &str) -> String {
        match self.hw_type {
            HwEncoder::Nvenc => {
                // NVENC preset: p1 ~ p7
                match software_preset {
                    "ultrafast" => "p1",
                    "superfast" => "p2",
                    "veryfast" => "p3",
                    "faster" => "p4",
                    "fast" => "p5",
                    "medium" => "p6",
                    _ => "p7", // slow / slower / veryslow
                }
                .to_string()
            }
            HwEncoder::Qsv => {
                // QSV preset: veryfast / faster / fast / medium / slow / slower / veryslow
                match software_preset {
                    "ultrafast" | "superfast" => "veryfast",
                    "veryfast" => "veryfast",
                    "faster" => "faster",
                    "fast" => "fast",
                    "medium" => "medium",
                    "slow" => "slow",
                    "slower" => "slower",
                    "veryslow" => "veryslow",
                    _ => "medium",
                }
                .to_string()
            }
            HwEncoder::Amf => {
                // AMF quality preset: speed / balanced / quality
                match software_preset {
                    "ultrafast" | "superfast" | "veryfast" => "speed",
                    "faster" | "fast" | "medium" => "balanced",
                    _ => "quality",
                }
                .to_string()
            }
        }
    }

    /// 映射 CRF（0-51）到硬件编码器的质量参数
    /// NVENC: CQ +2 | QSV: ICQ +2 | AMF: QP +2
    pub fn map_quality_args(&self, crf: u32) -> Vec<String> {
        let adjusted = (crf + 2).min(51);
        match self.hw_type {
            HwEncoder::Nvenc => {
                vec!["-rc:v".into(), "vbr".into(), "-cq:v".into(), adjusted.to_string()]
            }
            HwEncoder::Qsv => {
                vec!["-global_quality:v".into(), adjusted.to_string()]
            }
            HwEncoder::Amf => {
                vec![
                    "-rc:v".into(),
                    "cqp".into(),
                    "-qp_i".into(),
                    adjusted.to_string(),
                    "-qp_p".into(),
                    adjusted.to_string(),
                ]
            }
        }
    }

    /// 将目标位深映射到硬件编码器支持的 4:2:0 像素格式。
    pub fn map_pix_fmt(&self, software_pix_fmt: &str) -> String {
        if software_pix_fmt.contains("10") {
            match self.hw_type {
                HwEncoder::Nvenc | HwEncoder::Qsv => "p010le",
                HwEncoder::Amf => "p010",
            }
            .to_string()
        } else {
            "nv12".to_string()
        }
    }
}
