//! 系统音频解码器检测（仅 Windows）。
//!
//! VRChat（AVPro）通过 Windows Media Foundation 解码音频。原生 Windows 不自带
//! AC-3 / E-AC-3 解码器（需安装 Dolby 解码器包），因此「白名单兼容」的文件仍可能
//! 因本机缺少解码器而无法在 VRChat 播放。本模块用 MFTEnumEx 枚举系统音频解码器，
//! 与 AVPro 的枚举路径一致；Store 安装的解码器是打包型 MFT，只出现在该 API 中
//!（注册表里查不到），故必须走 API 而非注册表。

#[cfg(windows)]
use std::collections::HashMap;
#[cfg(windows)]
use std::sync::Mutex;
#[cfg(windows)]
use std::time::{Duration, Instant};

/// 编码名 → 是否需要检测系统解码器（aac/mp3/opus 由应用内 ffmpeg 转码覆盖，
/// 系统普遍可解；Dolby 系列依赖系统组件）
#[cfg(windows)]
fn is_dolby_codec(codec: &str) -> bool {
    matches!(codec, "ac3" | "eac3")
}

#[cfg(not(windows))]
pub fn audio_decoder_available(_codec: &str) -> bool {
    true
}

#[cfg(windows)]
pub fn audio_decoder_available(codec: &str) -> bool {
    use windows::Win32::Media::MediaFoundation::{
        MFShutdown, MFStartup, MFTEnumEx, IMFActivate, MFMediaType_Audio, MFSTARTUP_LITE,
        MF_VERSION, MFT_CATEGORY_AUDIO_DECODER, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_LOCALMFT,
        MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_REGISTER_TYPE_INFO,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

    if !is_dolby_codec(codec) {
        return true;
    }
    // 检测失败按「有解码器」处理，避免误报引导用户安装
    let Some(subtype) = dolby_subtype(codec) else {
        return true;
    };

    // 结果短缓存：安装解码器后无需重启本应用即可刷新
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, (Instant, bool)>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(map) = cache.lock() {
        if let Some((at, ok)) = map.get(codec) {
            if at.elapsed() < Duration::from_secs(30) {
                return *ok;
            }
        }
    }

    let result = unsafe {
        // MTA 初始化可能因线程已有模式而失败，忽略即可
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        if MFStartup(MF_VERSION, MFSTARTUP_LITE).is_err() {
            true
        } else {
            let info = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Audio,
                guidSubtype: subtype,
            };
            let mut count = 0u32;
            let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
            let ok = MFTEnumEx(
                MFT_CATEGORY_AUDIO_DECODER,
                // 同时枚举同步与异步 MFT：Store 安装的 Dolby 解码器可能注册为异步 MFT，
                // 只查同步 MFT 会漏报为「缺少解码器」。
                MFT_ENUM_FLAG_SYNCMFT
                    | MFT_ENUM_FLAG_ASYNCMFT
                    | MFT_ENUM_FLAG_LOCALMFT
                    | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&info),
                None,
                &mut activates,
                &mut count,
            )
            .is_ok()
                && count > 0;
            if !activates.is_null() {
                // 数组由 CoTaskMemAlloc 分配，Rust 不会对其运行析构：
                // 必须逐个取出 IMFActivate 并释放（Drop → Release），否则泄漏 COM 引用
                for i in 0..count as usize {
                    let slot = activates.add(i);
                    if let Some(activate) = slot.read() {
                        drop(activate);
                    }
                }
                CoTaskMemFree(Some(activates.cast()));
            }
            let _ = MFShutdown();
            ok
        }
    };

    if let Ok(mut map) = cache.lock() {
        map.insert(codec.to_string(), (Instant::now(), result));
    }
    result
}

/// codec 名 → MF 音频子类型 GUID
#[cfg(windows)]
fn dolby_subtype(
    codec: &str,
) -> Option<windows::core::GUID> {
    use windows::Win32::Media::MediaFoundation::{
        MFAudioFormat_Dolby_AC3, MFAudioFormat_Dolby_DDPlus,
    };
    match codec {
        "ac3" => Some(MFAudioFormat_Dolby_AC3),
        "eac3" => Some(MFAudioFormat_Dolby_DDPlus),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 冒烟测试：打印当前系统的 Dolby 解码器可用性（不做断言，供人工诊断）。
    /// 运行：cargo test system_decoder_smoke -- --nocapture
    #[test]
    fn system_decoder_smoke() {
        println!("ac3 decoder available  = {}", audio_decoder_available("ac3"));
        println!("eac3 decoder available = {}", audio_decoder_available("eac3"));
        println!("aac (non-dolby)        = {}", audio_decoder_available("aac"));
    }
}
