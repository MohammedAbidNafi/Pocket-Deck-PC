//! Console-style UI sounds, the same ones as the phone (UiFeedback.kt there).
//!
//! Like on the phone, nothing ships as audio: the sounds are synthesised on first use into
//! small WAV files in `%LOCALAPPDATA%\PocketDeck\sounds` and played with Windows' own
//! `PlaySound`, so no audio library is loaded and nothing stays in memory.

use std::f64::consts::PI;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sound {
    Move,
    Select,
    Back,
    Toggle,
    Intro,
}

static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

impl Sound {
    pub fn from_name(name: &str) -> Option<Sound> {
        Some(match name {
            "move" => Sound::Move,
            "select" => Sound::Select,
            "back" => Sound::Back,
            "toggle" => Sound::Toggle,
            "intro" => Sound::Intro,
            _ => return None,
        })
    }

    fn file_name(self) -> &'static str {
        match self {
            Sound::Move => "move.wav",
            Sound::Select => "select.wav",
            Sound::Back => "back.wav",
            Sound::Toggle => "toggle.wav",
            // Same name as the phone's cache file; bump it whenever the sound changes.
            Sound::Intro => "intro_epic_v2.wav",
        }
    }

    fn synthesise(self) -> Vec<i16> {
        match self {
            Sound::Move => blips(&[1760.0], 0.028, 0.18),
            Sound::Select => blips(&[880.0, 1320.0], 0.09, 0.22),
            Sound::Back => blips(&[990.0, 660.0], 0.08, 0.2),
            Sound::Toggle => blips(&[1320.0], 0.05, 0.2),
            Sound::Intro => epic_intro(RATE).iter().map(|v| (v * i16::MAX as f64) as i16).collect(),
        }
    }
}

const RATE: u32 = 44_100;

fn dir() -> PathBuf {
    pocketdeck_core::paths::local_dir().join("sounds")
}

/// Path of the sound's WAV file, writing it first if needed. Slow the first time for the intro
/// (about a tenth of a second in a release build), instant after that.
pub fn prepare(sound: Sound) -> Option<PathBuf> {
    let path = dir().join(sound.file_name());
    if !path.is_file() {
        std::fs::create_dir_all(dir()).ok()?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, wav(&sound.synthesise(), RATE)).ok()?;
        std::fs::rename(&tmp, &path).ok()?;
    }
    Some(path)
}

/// Writes every sound up front, intro first (needed straight away on start).
pub fn prepare_all() {
    for s in [Sound::Intro, Sound::Select, Sound::Move, Sound::Back, Sound::Toggle] {
        prepare(s);
    }
}

pub fn play(sound: Sound) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(path) = prepare(sound) else { return };
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: NUL-terminated path; PlaySound copies what it needs before returning (SND_ASYNC).
    unsafe {
        PlaySoundW(wide.as_ptr(), std::ptr::null_mut(), SND_FILENAME | SND_ASYNC | SND_NODEFAULT);
    }
}

/// A short sine blip (tones played back to back), as on the phone.
fn blips(tones: &[f64], seconds: f64, volume: f64) -> Vec<i16> {
    let rate = RATE as f64;
    let per_tone = (rate * seconds / tones.len() as f64) as usize;
    let mut samples = vec![0i16; per_tone * tones.len()];
    for (t, freq) in tones.iter().enumerate() {
        for i in 0..per_tone {
            let time = i as f64 / rate;
            let attack = (i as f64 / (rate * 0.002)).min(1.0);
            let decay = (-time * 6.0 / (seconds / tones.len() as f64)).exp();
            samples[t * per_tone + i] =
                ((2.0 * PI * freq * time).sin() * attack * decay * volume * i16::MAX as f64) as i16;
        }
    }
    samples
}

/// `java.util.Random`, so the noise in the intro is sample-for-sample the phone's.
struct JavaRandom {
    seed: u64,
    next_gaussian: Option<f64>,
}

impl JavaRandom {
    const MULT: u64 = 0x5DEECE66D;
    const MASK: u64 = (1 << 48) - 1;

    fn new(seed: i64) -> Self {
        JavaRandom { seed: (seed as u64 ^ Self::MULT) & Self::MASK, next_gaussian: None }
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.seed = (self.seed.wrapping_mul(Self::MULT).wrapping_add(0xB)) & Self::MASK;
        (self.seed >> (48 - bits)) as i32
    }

    fn next_double(&mut self) -> f64 {
        let hi = (self.next(26) as i64) << 27;
        let lo = self.next(27) as i64;
        (hi + lo) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    fn next_gaussian(&mut self) -> f64 {
        if let Some(g) = self.next_gaussian.take() {
            return g;
        }
        loop {
            let v1 = 2.0 * self.next_double() - 1.0;
            let v2 = 2.0 * self.next_double() - 1.0;
            let s = v1 * v1 + v2 * v2;
            if s < 1.0 && s != 0.0 {
                let m = (-2.0 * s.ln() / s).sqrt();
                self.next_gaussian = Some(v2 * m);
                return v1 * m;
            }
        }
    }
}

/// The cinematic startup sound, timed to the logo animation (seconds):
///  - 0.00 a dark rising swell of filtered noise and a low drone as the phone appears,
///  - 0.40 the grips snap on: a big sub boom dropping in pitch, plus a punchy thump,
///  - 0.78 the Android head pops up: a wide brass-like power-chord stab,
///
/// all ringing out through a reverb. A straight port of `epicIntro` in UiFeedback.kt.
fn epic_intro(rate: u32) -> Vec<f64> {
    let r = rate as f64;
    let length = 3.2;
    let n = (r * length) as usize;
    let mut dry = vec![0.0f64; n];
    let mut rnd = JavaRandom::new(7);
    let at = |seconds: f64| (seconds * r) as usize;

    // 1. Rising swell: C1/C2 drone plus noise through a one-pole low-pass that opens up.
    {
        let mut lp = 0.0;
        let end = at(0.45);
        for (i, sample) in dry.iter_mut().enumerate().take(end) {
            let t = i as f64 / r;
            let p = t / 0.45;
            let cutoff = 0.004 + 0.05 * p * p;
            lp += cutoff * (rnd.next_gaussian() - lp);
            let drone = (2.0 * PI * 32.7 * t).sin() + 0.6 * (2.0 * PI * 65.4 * t).sin();
            let env = p * p;
            *sample += (lp * 2.2 + drone * 0.35) * env * 0.55;
        }
    }

    // 2. Sub boom on the grips: a sine sweeping 120 Hz down to 34 Hz, plus a noise thump.
    {
        let start = at(0.40);
        let mut phase = 0.0;
        for i in 0..at(1.9) {
            let t = i as f64 / r;
            let freq = 34.0 + 86.0 * (-t * 9.0).exp();
            phase += 2.0 * PI * freq / r;
            let env = (t / 0.004).min(1.0) * (-t * 1.7).exp();
            let thump = if t < 0.06 { rnd.next_gaussian() * (-t * 70.0).exp() * 0.8 } else { 0.0 };
            let idx = start + i;
            if idx < n {
                dry[idx] += ((phase.sin() * 2.4).tanh() * 1.1 + thump) * env;
            }
        }
    }

    // 3. Brass-like chord stab on the Android head: C2 G2 C3 G3 C4 E4, detuned saw-ish voices.
    {
        let start = at(0.78);
        let chord = [65.41, 98.0, 130.81, 196.0, 261.63, 329.63];
        let gains = [0.9, 0.6, 0.55, 0.4, 0.3, 0.22];
        for i in 0..at(2.3) {
            let t = i as f64 / r;
            let attack = (t / 0.03).min(1.0);
            let env = attack * (0.35 + 0.65 * (-t * 3.2).exp()) * (-t * 0.9).exp();
            let bright = 3.0 + 7.0 * (-t * 4.0).exp();
            let mut v = 0.0;
            for (k, f) in chord.iter().enumerate() {
                for detune in [0.997, 1.003] {
                    for h in 1..=10 {
                        let rolloff = (-((h - 1) as f64) / bright).exp();
                        v += (2.0 * PI * f * detune * h as f64 * t).sin() * rolloff / h as f64 * gains[k] * 0.5;
                    }
                }
            }
            let idx = start + i;
            if idx < n {
                dry[idx] += (v * 0.55).tanh() * env * 0.9;
            }
        }
        // A soft high shimmer (C6 + G6) to finish the hit.
        for i in 0..at(1.6) {
            let t = i as f64 / r;
            let env = (t / 0.08).min(1.0) * (-t * 2.2).exp();
            let idx = start + at(0.05) + i;
            if idx < n {
                dry[idx] += ((2.0 * PI * 1046.5 * t).sin() + 0.7 * (2.0 * PI * 1568.0 * t).sin()) * env * 0.05;
            }
        }
    }

    // 4. Reverb: four damped feedback combs in parallel for a big-room tail.
    let mut wet = vec![0.0f64; n];
    for delay_ms in [29.7, 37.1, 41.1, 43.7] {
        let d = (delay_ms / 1000.0 * r) as usize;
        let mut buf = vec![0.0f64; n];
        let mut damp = 0.0;
        for i in 0..n {
            let back = if i >= d { buf[i - d] } else { 0.0 };
            damp = damp * 0.4 + back * 0.6;
            buf[i] = dry[i] + damp * 0.84;
            wet[i] += buf[i] * 0.25;
        }
    }

    // Mix, saturate, normalise and fade the very end.
    let mut out: Vec<f64> = (0..n).map(|i| ((dry[i] + wet[i] * 0.45) * 1.3).tanh()).collect();
    let peak = out.iter().fold(0.0f64, |m, v| m.max(v.abs())).max(1e-6);
    for (i, v) in out.iter_mut().enumerate() {
        let tail = ((n - i) as f64 / (r * 0.4)).min(1.0);
        *v = *v / peak * 0.95 * tail;
    }
    out
}

/// 16-bit mono PCM WAV.
fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_random_matches_the_jdk() {
        // Reference values printed by a real JDK for `new java.util.Random(7)`.
        let mut r = JavaRandom::new(7);
        assert_eq!(r.next(32), -1156638823);
        let mut r = JavaRandom::new(7);
        assert_eq!(r.next_double(), 0.7306990420600421);
        let mut r = JavaRandom::new(7);
        let g = [r.next_gaussian(), r.next_gaussian(), r.next_gaussian()];
        assert_eq!(g, [0.8452060657049847, 0.9128761787534405, -0.2870786364749953]);
    }

    #[test]
    fn intro_is_three_seconds_and_normalised() {
        let s = epic_intro(RATE);
        assert_eq!(s.len(), (RATE as f64 * 3.2) as usize);
        let peak = s.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        assert!(peak > 0.9 && peak <= 0.95 + 1e-9);
    }

    #[test]
    fn wav_header_is_well_formed() {
        let w = wav(&[0, 1, -1], RATE);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 6);
    }
}
