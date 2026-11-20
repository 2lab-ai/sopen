//! sherpa-onnx's offline source separation through its C API (`c-api.h`, v2.0.2), statically
//! linked by build.rs. sherpa-onnx's Rust crates do not bind source separation, so the few
//! declarations are written here. UVR's `Process` returns two stems: the model's target (the
//! voice, for UVR-MDX-NET-Voc_FT) and the input minus it (the music).

use anyhow::{bail, Context, Result};
use std::ffi::CString;
use std::os::raw::{c_char, c_float};
use std::path::Path;

#[repr(C)]
struct SpleeterModelConfig {
    vocals: *const c_char,
    accompaniment: *const c_char,
}

#[repr(C)]
struct UvrModelConfig {
    model: *const c_char,
}

#[repr(C)]
struct ModelConfig {
    spleeter: SpleeterModelConfig,
    uvr: UvrModelConfig,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
}

#[repr(C)]
struct Config {
    model: ModelConfig,
}

#[repr(C)]
struct Stem {
    samples: *mut *mut c_float,
    num_channels: i32,
    n: i32,
}

#[repr(C)]
struct Output {
    stems: *const Stem,
    num_stems: i32,
    sample_rate: i32,
}

enum Opaque {}

extern "C" {
    fn SherpaOnnxCreateOfflineSourceSeparation(config: *const Config) -> *const Opaque;
    fn SherpaOnnxDestroyOfflineSourceSeparation(ss: *const Opaque);
    fn SherpaOnnxOfflineSourceSeparationGetOutputSampleRate(ss: *const Opaque) -> i32;
    fn SherpaOnnxOfflineSourceSeparationProcess(
        ss: *const Opaque,
        samples: *const *const c_float,
        num_channels: i32,
        num_samples: i32,
        sample_rate: i32,
    ) -> *const Output;
    fn SherpaOnnxDestroySourceSeparationOutput(p: *const Output);
}

/// A loaded UVR model. One window at a time (`&mut self`); `Process` cannot be interrupted.
pub struct Engine {
    ptr: *const Opaque,
    rate: u32,
}

/// One separated window, planar: `[left, right]` per stem, each as long as the input.
pub struct Separated {
    pub voice: [Vec<f32>; 2],
    pub music: [Vec<f32>; 2],
}

impl Engine {
    /// Loads `model` (an already verified UVR ONNX file) on the CPU with `threads` threads.
    pub fn new(model: &Path, threads: usize) -> Result<Self> {
        let path = CString::new(model.as_os_str().as_encoded_bytes())
            .context("the model path holds a NUL byte")?;
        let empty = CString::default();
        let provider = CString::new("cpu").expect("no NUL");
        let config = Config {
            model: ModelConfig {
                spleeter: SpleeterModelConfig {
                    vocals: empty.as_ptr(),
                    accompaniment: empty.as_ptr(),
                },
                uvr: UvrModelConfig {
                    model: path.as_ptr(),
                },
                num_threads: i32::try_from(threads.max(1)).unwrap_or(i32::MAX),
                debug: 0,
                provider: provider.as_ptr(),
            },
        };
        // SAFETY: `config` and the strings it points at live across the call; sherpa copies
        // what it keeps.
        let ptr = unsafe { SherpaOnnxCreateOfflineSourceSeparation(&config) };
        if ptr.is_null() {
            bail!("the separation engine could not load {}", model.display());
        }
        // SAFETY: `ptr` is a live engine.
        let rate = unsafe { SherpaOnnxOfflineSourceSeparationGetOutputSampleRate(ptr) };
        let engine = Self {
            ptr,
            rate: u32::try_from(rate).unwrap_or(0),
        };
        if engine.rate == 0 {
            bail!("{} reports no sample rate", model.display());
        }
        Ok(engine)
    }

    /// The model's sample rate: input at this rate is not resampled, and output comes at it.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Separates one stereo window at [`Engine::rate`].
    pub fn separate(&mut self, left: &[f32], right: &[f32]) -> Result<Separated> {
        if left.len() != right.len() || left.is_empty() {
            bail!("a window needs two channels of equal, non-zero length");
        }
        let n = i32::try_from(left.len()).context("window too long")?;
        let channels = [left.as_ptr(), right.as_ptr()];
        // SAFETY: two live channels of `n` samples each; the rate is the engine's own.
        let out = unsafe {
            SherpaOnnxOfflineSourceSeparationProcess(
                self.ptr,
                channels.as_ptr(),
                2,
                n,
                self.rate as i32,
            )
        };
        if out.is_null() {
            bail!("the separation engine failed on a window");
        }
        // SAFETY: `out` is live until destroyed below; its stems and channels are arrays of
        // the lengths it states.
        let result = unsafe {
            let o = &*out;
            let stems = if o.num_stems == 2 && !o.stems.is_null() {
                Some(std::slice::from_raw_parts(o.stems, 2))
            } else {
                None
            };
            let copy = |stem: &Stem| -> Option<[Vec<f32>; 2]> {
                if stem.num_channels != 2 || stem.samples.is_null() || stem.n < 0 {
                    return None;
                }
                let channel = |c: usize| {
                    std::slice::from_raw_parts(*stem.samples.add(c), stem.n as usize).to_vec()
                };
                Some([channel(0), channel(1)])
            };
            let separated = stems.and_then(|s| {
                Some(Separated {
                    voice: copy(&s[0])?,
                    music: copy(&s[1])?,
                })
            });
            SherpaOnnxDestroySourceSeparationOutput(out);
            separated
        };
        result.context("the separation engine returned an unexpected shape (not 2 stereo stems)")
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: created by `SherpaOnnxCreateOfflineSourceSeparation`, destroyed once.
        unsafe { SherpaOnnxDestroyOfflineSourceSeparation(self.ptr) };
    }
}
