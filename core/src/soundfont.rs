use std::{marker::PhantomData, path::PathBuf, sync::Arc};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use simdeez::Simd;
use to_vec::ToVec;

use self::audio::AudioFileLoader;

use super::{
    voice::VoiceControlData,
    voice::{
        BufferSamplers, EnvelopeParameters, SIMDConstant, SIMDNearestSampleGrabber,
        SIMDStereoVoice, SIMDStereoVoiceSampler, SIMDVoiceControl, SIMDVoiceEnvelope, SampleReader,
        Voice, VoiceBase, VoiceCombineSIMD,
    },
};
use crate::{helpers::FREQS, voice::EnvelopeDescriptor, AudioStreamParams};

pub mod audio;

pub trait VoiceSpawner: Sync + Send {
    fn spawn_voice(&self, control: &VoiceControlData) -> Box<dyn Voice>;
}

pub trait SoundfontBase: Sync + Send + std::fmt::Debug {
    fn stream_params<'a>(&'a self) -> &'a AudioStreamParams;

    fn get_attack_voice_spawners_at(&self, key: u8, vel: u8) -> Vec<Box<dyn VoiceSpawner>>;
    fn get_release_voice_spawners_at(&self, key: u8, vel: u8) -> Vec<Box<dyn VoiceSpawner>>;
}

// pub struct SineVoice {
//     freq: f64,

//     amp: f32,
//     phase: f64,
// }

// impl SineVoice {
//     pub fn spawn(key: u8, vel: u8, sample_rate: u32) -> Self {
//         let freq = (FREQS[key as usize] as f64 / sample_rate as f64) * std::f64::consts::PI;
//         let amp = 1.04f32.powf(vel as f32 - 127.0);

//         Self {
//             freq,
//             amp,
//             phase: 0.0,
//         }
//     }
// }

// impl Voice for SineVoice {
//     fn is_ended(&self) -> bool {
//         self.amp == 0.0
//     }

//     fn is_releasing(&self) -> bool {
//         self.is_ended()
//     }

//     fn signal_release(&mut self) {
//         self.amp = 0.0;
//     }

//     fn render_to(&mut self, out: &mut [f32]) {
//         for i in 0..out.len() {
//             let sample = self.phase.cos() as f32;
//             let sample = if sample > 0.0 { 1.0 } else { -1.0 };
//             let sample = self.amp * sample;
//             self.phase += self.freq;
//             out[i] += sample;
//         }
//     }
// }

struct SampledVoiceSpawner<S: 'static + Simd + Send + Sync> {
    base_freq: f32,
    amp: f32,
    volume_envelope_params: Arc<EnvelopeParameters>,
    samples: Vec<Arc<[f32]>>,
    vel: u8,
    _s: PhantomData<S>,
}

impl<S: Simd + Send + Sync> SampledVoiceSpawner<S> {
    pub fn new(
        key: u8,
        vel: u8,
        sample_rate_fac: f32,
        volume_envelope_params: Arc<EnvelopeParameters>,
        sf: &SquareSoundfont,
    ) -> Self {
        let amp = 1.04f32.powf(vel as f32 - 127.0);

        let (samples, base_freq) = if key < 21 {
            let samples = sf.samples[0].clone();
            let base_freq = FREQS[key as usize] / FREQS[21];
            (samples, base_freq * sample_rate_fac)
        } else if key > 108 {
            let samples = sf.samples.last().unwrap().clone();
            let base_freq = FREQS[key as usize] / FREQS[108];
            (samples, base_freq * sample_rate_fac)
        } else {
            let samples = sf.samples[key as usize - 21].clone();
            let base_freq = 1.0;
            (samples, base_freq * sample_rate_fac)
        };

        Self {
            base_freq,
            amp,
            volume_envelope_params,
            samples,
            vel,
            _s: PhantomData,
        }
    }
}

impl<S: 'static + Sync + Send + Simd> VoiceSpawner for SampledVoiceSpawner<S> {
    fn spawn_voice(&self, control: &VoiceControlData) -> Box<dyn Voice> {
        let pitch_fac = SIMDConstant::<S>::new(self.base_freq as f32);

        let pitch_multiplier = SIMDVoiceControl::new(control, |vc| vc.voice_pitch_multiplier);

        let pitch_fac = VoiceCombineSIMD::mult(pitch_fac, pitch_multiplier);

        let left = SIMDNearestSampleGrabber::new(SampleReader::new(BufferSamplers::new_f32(
            self.samples[0].clone(),
        )));
        let right = SIMDNearestSampleGrabber::new(SampleReader::new(BufferSamplers::new_f32(
            self.samples[1].clone(),
        )));

        let sampler = SIMDStereoVoiceSampler::new(left, right, pitch_fac);

        let amp = SIMDConstant::<S>::new(self.amp);
        let volume_envelope = SIMDVoiceEnvelope::new(self.volume_envelope_params.clone());

        let modulated = VoiceCombineSIMD::mult(amp, sampler);
        let modulated = VoiceCombineSIMD::mult(volume_envelope, modulated);

        let flattened = SIMDStereoVoice::new(modulated);
        let base = VoiceBase::new(self.vel, flattened);

        Box::new(base)
    }
}

#[derive(Debug)]
pub struct SquareSoundfont {
    samples: Vec<Vec<Arc<[f32]>>>,
    volume_envelope_params: Arc<EnvelopeParameters>,
    stream_params: AudioStreamParams,
}

impl SquareSoundfont {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let samples = (21..109).to_vec().par_iter()
            .map(|i| {
                println!("Loading {}", i);
                AudioFileLoader::load_wav(&PathBuf::from(format!(
                    "D:/Midis/Steinway-B-211-master/Steinway-B-211-master/Samples/KEPSREC{:0>3}.wav",
                    i
                )))
                .unwrap()
            })
            .collect();

        let envelope_descriptor = EnvelopeDescriptor {
            start_percent: 0.0,
            delay: 0.0,
            attack: 0.0,
            hold: 0.0,
            decay: 0.1,
            sustain_percent: 0.7,
            release: 0.2,
        };

        let volume_envelope_params = Arc::new(envelope_descriptor.to_envelope_params(sample_rate));

        Self {
            samples,
            volume_envelope_params,
            stream_params: AudioStreamParams::new(sample_rate, channels),
        }
    }
}

impl SoundfontBase for SquareSoundfont {
    fn stream_params<'a>(&'a self) -> &'a AudioStreamParams {
        &self.stream_params
    }

    fn get_attack_voice_spawners_at(&self, key: u8, vel: u8) -> Vec<Box<dyn VoiceSpawner>> {
        use simdeez::*; // nuts

        use simdeez::avx2::*;
        use simdeez::scalar::*;
        use simdeez::sse2::*;
        use simdeez::sse41::*;

        simd_runtime_generate!(
            fn get(key: u8, vel: u8, sf: &SquareSoundfont) -> Vec<Box<dyn VoiceSpawner>> {
                let sr = 96000.0 / sf.stream_params.sample_rate as f32;

                vec![Box::new(SampledVoiceSpawner::<S>::new(
                    key,
                    vel,
                    sr,
                    sf.volume_envelope_params.clone(),
                    sf,
                ))]
            }
        );

        get_runtime_select(key, vel, &self)
    }

    fn get_release_voice_spawners_at(&self, _key: u8, _vel: u8) -> Vec<Box<dyn VoiceSpawner>> {
        vec![]
    }
}
