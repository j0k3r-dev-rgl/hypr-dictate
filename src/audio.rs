use anyhow::{Context, Result, bail, ensure};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Debug, PartialEq)]
pub struct Microphone {
    pub name: String,
    pub description: String,
}

pub fn microphones() -> Vec<Microphone> {
    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };

    let default_name = host
        .default_input_device()
        .and_then(|d| d.description().ok())
        .map(|desc| desc.name().to_string())
        .unwrap_or_default();

    let mut list: Vec<Microphone> = devices
        .filter_map(|d| {
            let desc = d.description().ok()?;
            let name = desc.name().to_string();
            if name.is_empty() {
                return None;
            }
            Some(Microphone {
                description: if name == default_name {
                    format!("{name} (Predeterminado)")
                } else {
                    name.clone()
                },
                name,
            })
        })
        .collect();

    list.sort_by(|a, b| a.name.cmp(&b.name));
    list.dedup_by(|a, b| a.name == b.name);
    // Sort default device first
    list.sort_by_key(|m| m.name != default_name);
    list
}

pub struct ActiveCapture {
    _stream: Box<dyn StreamTrait>,
    running: Arc<AtomicBool>,
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl ActiveCapture {
    pub fn stop(self) -> Result<Vec<u8>> {
        self.running.store(false, Ordering::SeqCst);
        // Small pause to let stream flush
        thread::sleep(Duration::from_millis(50));
        let buf = self.buffer.lock().unwrap().clone();
        Ok(buf)
    }
}

pub fn start_recording(
    device_name: &str,
    level: Arc<AtomicU32>,
) -> Result<ActiveCapture> {
    let host = cpal::default_host();
    let device = if !device_name.is_empty() {
        host.input_devices()?
            .find(|d| {
                d.description()
                    .map(|desc| desc.name() == device_name)
                    .unwrap_or(false)
            })
            .or_else(|| host.default_input_device())
            .context("No se encontró el dispositivo de audio especificado")?
    } else {
        host.default_input_device()
            .context("No hay micrófono predeterminado disponible")?
    };

    let config = device
        .default_input_config()
        .context("No se pudo obtener la configuración del micrófono")?;

    let sample_rate = config.sample_rate();
    let channels = config.channels() as usize;
    ensure!(channels > 0, "Canales de audio inválidos");

    let running = Arc::new(AtomicBool::new(true));
    let buffer = Arc::new(Mutex::new(Vec::with_capacity(32000 * 5))); // 5 secs initial capacity

    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();

    let stream: Box<dyn StreamTrait> = match sample_format {
        cpal::SampleFormat::F32 => {
            let stream = build_input_stream_f32(
                &device,
                stream_config,
                sample_rate,
                channels,
                buffer.clone(),
                running.clone(),
                level,
            )?;
            Box::new(stream)
        }
        cpal::SampleFormat::I16 => {
            let stream = build_input_stream_i16(
                &device,
                stream_config,
                sample_rate,
                channels,
                buffer.clone(),
                running.clone(),
                level,
            )?;
            Box::new(stream)
        }
        _ => bail!("Formato de audio no soportado: {:?}", sample_format),
    };

    stream.play().context("No se pudo iniciar la captura de audio")?;

    Ok(ActiveCapture {
        _stream: stream,
        running,
        buffer,
    })
}

struct Resampler {
    in_rate: f64,
    out_rate: f64,
    channels: usize,
    pos: f64,
    prev_sample: f32,
    peak_accum: u32,
    samples_since_peak: usize,
}

impl Resampler {
    fn new(in_rate: u32, channels: usize) -> Self {
        Self {
            in_rate: in_rate as f64,
            out_rate: 16000.0,
            channels,
            pos: 0.0,
            prev_sample: 0.0,
            peak_accum: 0,
            samples_since_peak: 0,
        }
    }

    fn process_frame<F>(&mut self, frame: &[f32], mut out_pcm: F) -> Option<u32>
    where
        F: FnMut(i16),
    {
        // Downmix to mono
        let mut mono = 0.0f32;
        for &s in frame.iter().take(self.channels) {
            mono += s;
        }
        mono /= self.channels as f32;

        let step = self.in_rate / self.out_rate;
        let mut reported_peak = None;

        while self.pos < 1.0 {
            // Linear interpolation between prev_sample and mono
            let t = self.pos as f32;
            let sample = self.prev_sample + (mono - self.prev_sample) * t;
            let sample_clamped = sample.clamp(-1.0, 1.0);
            let s_i16 = (sample_clamped * 32767.0) as i16;
            out_pcm(s_i16);

            let abs = s_i16.unsigned_abs() as u32;
            if abs > self.peak_accum {
                self.peak_accum = abs;
            }
            self.samples_since_peak += 1;
            if self.samples_since_peak >= 800 {
                // ~50ms
                reported_peak = Some(self.peak_accum);
                self.peak_accum = 0;
                self.samples_since_peak = 0;
            }

            self.pos += step;
        }

        self.pos -= 1.0;
        self.prev_sample = mono;
        reported_peak
    }
}

fn build_input_stream_f32(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    in_rate: u32,
    channels: usize,
    buffer: Arc<Mutex<Vec<u8>>>,
    running: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
) -> Result<cpal::Stream> {
    let mut resampler = Resampler::new(in_rate, channels);

    let stream = device.build_input_stream(
        config,
        move |data: &[f32], _| {
            if !running.load(Ordering::Relaxed) {
                return;
            }
            let mut out = Vec::new();
            let mut latest_peak = None;
            for frame in data.chunks_exact(channels) {
                if let Some(p) = resampler.process_frame(frame, |s| {
                    out.extend_from_slice(&s.to_le_bytes());
                }) {
                    latest_peak = Some(p);
                }
            }

            if let Some(p) = latest_peak {
                level.store(p, Ordering::Relaxed);
            }

            if !out.is_empty() {
                let mut buf = buffer.lock().unwrap();
                if buf.len() + out.len() <= 9_600_000 {
                    buf.extend_from_slice(&out);
                }
            }
        },
        move |err| {
            eprintln!("Error en captura de audio: {err}");
        },
        None,
    )?;

    Ok(stream)
}

fn build_input_stream_i16(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    in_rate: u32,
    channels: usize,
    buffer: Arc<Mutex<Vec<u8>>>,
    running: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
) -> Result<cpal::Stream> {
    let mut resampler = Resampler::new(in_rate, channels);

    let stream = device.build_input_stream(
        config,
        move |data: &[i16], _| {
            if !running.load(Ordering::Relaxed) {
                return;
            }
            let mut out = Vec::new();
            let mut latest_peak = None;
            for frame in data.chunks_exact(channels) {
                let f_frame: Vec<f32> = frame.iter().map(|&s| s as f32 / 32768.0).collect();
                if let Some(p) = resampler.process_frame(&f_frame, |s| {
                    out.extend_from_slice(&s.to_le_bytes());
                }) {
                    latest_peak = Some(p);
                }
            }

            if let Some(p) = latest_peak {
                level.store(p, Ordering::Relaxed);
            }

            if !out.is_empty() {
                let mut buf = buffer.lock().unwrap();
                if buf.len() + out.len() <= 9_600_000 {
                    buf.extend_from_slice(&out);
                }
            }
        },
        move |err| {
            eprintln!("Error en captura de audio: {err}");
        },
        None,
    )?;

    Ok(stream)
}
