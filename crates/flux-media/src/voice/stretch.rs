//! Faster speech at the same pitch: WSOLA (waveform-similarity overlap-add). The input is cut into
//! frames of 20 ms overlapping by half, taken `speed` times further apart than they are put down;
//! each frame moves (within ±7.5 ms) to where its start looks most like the continuation of the
//! previous frame, so the joins fall in step with the voice's periods and neither click nor echo.
//! At 1× the frames are taken where they are put down: the sound comes out as it went in.

/// Half a frame, the step the frames are put down at: 10 ms at 48 kHz.
pub(crate) const HOP: usize = 480;
const FRAME: usize = 2 * HOP;
/// How far a frame may move from where the speed puts it: 7.5 ms, more than half the period of
/// the lowest voices (~70 Hz).
const TOLERANCE: usize = 360;
/// The coarse search looks at every 4th sample and offset; the fine one at every sample around
/// the coarse best.
const COARSE: usize = 4;

pub(crate) struct Stretch {
    speed: f64,
    /// A Hann window: two of them half a frame apart add up to one.
    window: Vec<f32>,
    /// The input still needed; `input[0]` is the input's sample `base`.
    input: Vec<f32>,
    base: usize,
    /// No more input comes: past its end it is silence.
    finished: bool,
    /// Where the speed puts the next frame, in the input's samples.
    ideal: f64,
    /// Where the previous frame was taken (`None` before the first).
    previous: Option<usize>,
    /// The previous frame's second half, windowed: the next frame's first half adds to it.
    tail: Vec<f32>,
    /// The last tail went out: nothing more comes.
    done: bool,
}

/// A step of the output: `HOP` samples (fewer at the very end) and the stretch of the input they
/// stand for, in the input's samples.
pub(crate) struct Piece {
    pub samples: Vec<f32>,
    pub from: f64,
    pub to: f64,
}

impl Stretch {
    pub fn new(speed: f32) -> Self {
        let window = (0..FRAME)
            .map(|n| {
                0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / FRAME as f64).cos() as f32
            })
            .collect();
        Stretch {
            speed: clamp(speed),
            window,
            input: Vec::new(),
            base: 0,
            finished: false,
            ideal: 0.0,
            previous: None,
            tail: vec![0.0; HOP],
            done: false,
        }
    }

    /// Takes effect from the next frame.
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = clamp(speed);
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.input.extend_from_slice(samples);
    }

    /// The input ended: what is left comes out, then nothing.
    pub fn finish(&mut self) {
        self.finished = true;
    }

    /// The next step of the output, if the input is there for it.
    pub fn pull(&mut self) -> Option<Piece> {
        if self.done {
            return None;
        }
        let end = self.base + self.input.len();
        let ideal = self.ideal.round() as usize;
        if self.finished && ideal >= end {
            // The last frame's second half fades out the sound.
            self.done = true;
            let from = self.ideal;
            return Some(Piece {
                samples: std::mem::take(&mut self.tail),
                from,
                to: from,
            });
        }
        if !self.finished && ideal + TOLERANCE + FRAME + COARSE > end {
            return None;
        }
        let at = match self.previous {
            // The first frame: its first half comes out as it is.
            None => {
                self.tail = (0..HOP)
                    .map(|index| self.input_at(index) * self.window[HOP + index])
                    .collect();
                0
            }
            // 1×: the natural continuation, with the speed's place following it.
            Some(previous) if self.speed == 1.0 => {
                self.ideal = (previous + HOP) as f64;
                previous + HOP
            }
            Some(previous) => self.best_start(previous + HOP, ideal),
        };
        let mut samples = std::mem::take(&mut self.tail);
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample += self.input_at(at + index) * self.window[index];
        }
        self.tail = (HOP..FRAME)
            .map(|index| self.input_at(at + index) * self.window[index])
            .collect();
        let from = self.ideal;
        self.ideal += HOP as f64 * self.speed;
        self.previous = Some(at);

        // What no frame to come needs.
        let keep = at.min((self.ideal as usize).saturating_sub(TOLERANCE + COARSE));
        if keep > self.base + 8 * FRAME {
            self.input.drain(..keep - self.base);
            self.base = keep;
        }
        Some(Piece {
            samples,
            from,
            to: self.ideal,
        })
    }

    /// The input's sample `index`; silence past the end.
    fn input_at(&self, index: usize) -> f32 {
        index
            .checked_sub(self.base)
            .and_then(|at| self.input.get(at))
            .copied()
            .unwrap_or(0.0)
    }

    /// Where near `ideal` a frame's first half looks most like the input from `continuation` on:
    /// the normalized cross-correlation, coarse then fine.
    fn best_start(&self, continuation: usize, ideal: usize) -> usize {
        let lowest = ideal.saturating_sub(TOLERANCE).max(self.base);
        let highest = ideal + TOLERANCE;
        let score = |start: usize, step: usize| {
            let (mut product, mut energy) = (0.0f32, 1e-9f32);
            for index in (0..HOP).step_by(step) {
                let candidate = self.input_at(start + index);
                product += self.input_at(continuation + index) * candidate;
                energy += candidate * candidate;
            }
            product / energy.sqrt()
        };
        let mut best = (ideal, f32::MIN);
        for start in (lowest..=highest).step_by(COARSE) {
            let value = score(start, COARSE);
            if value > best.1 {
                best = (start, value);
            }
        }
        let around = best.0;
        best.1 = f32::MIN;
        for start in around.saturating_sub(COARSE - 1).max(lowest)..=(around + COARSE - 1) {
            let value = score(start, 1);
            if value > best.1 {
                best = (start, value);
            }
        }
        best.0
    }
}

fn clamp(speed: f32) -> f64 {
    f64::from(speed.clamp(0.5, 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stretch(input: &[f32], speed: f32) -> Vec<f32> {
        let mut stretch = Stretch::new(speed);
        let mut output = Vec::new();
        for chunk in input.chunks(960) {
            stretch.push(chunk);
            while let Some(piece) = stretch.pull() {
                output.extend(piece.samples);
            }
        }
        stretch.finish();
        while let Some(piece) = stretch.pull() {
            output.extend(piece.samples);
        }
        output
    }

    fn tone(hertz: f32, seconds: f32) -> Vec<f32> {
        (0..(48_000.0 * seconds) as usize)
            .map(|n| 0.5 * (2.0 * std::f32::consts::PI * hertz * n as f32 / 48_000.0).sin())
            .collect()
    }

    #[test]
    fn as_it_was_at_one() {
        let input = tone(220.0, 0.5);
        let output = stretch(&input, 1.0);
        assert!(output.len() >= input.len());
        for (index, (a, b)) in input.iter().zip(&output).enumerate() {
            assert!((a - b).abs() < 1e-5, "sample {index}: {a} ≠ {b}");
        }
    }

    #[test]
    fn length_follows_the_speed() {
        let input = tone(180.0, 3.0);
        for speed in [0.5, 1.5, 2.0, 3.0] {
            let output = stretch(&input, speed);
            let expected = input.len() as f32 / speed;
            let ratio = output.len() as f32 / expected;
            assert!(
                (ratio - 1.0).abs() < 0.02,
                "{speed}×: {} samples for {expected}",
                output.len()
            );
        }
    }

    /// The joins neither click (a jump larger than the tone's own steps) nor dip (frames out of
    /// phase cancel each other).
    #[test]
    fn joins_are_smooth() {
        for hertz in [110.0, 220.0, 345.0] {
            let input = tone(hertz, 2.0);
            let steepest = input
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).abs())
                .fold(0.0, f32::max);
            for speed in [1.5, 2.0] {
                let output = stretch(&input, speed);
                // Away from the fades at both ends.
                let middle = &output[FRAME..output.len() - 2 * FRAME];
                let jump = middle
                    .windows(2)
                    .map(|pair| (pair[1] - pair[0]).abs())
                    .fold(0.0, f32::max);
                assert!(
                    jump < steepest * 1.3,
                    "{hertz} Hz at {speed}×: a jump of {jump} (the tone's steepest {steepest})"
                );
                let period = (48_000.0 / hertz) as usize + 1;
                for window in middle
                    .chunks(period)
                    .filter(|window| window.len() == period)
                {
                    let peak = window.iter().map(|sample| sample.abs()).fold(0.0, f32::max);
                    assert!(
                        peak > 0.45 && peak < 0.55,
                        "{hertz} Hz at {speed}×: a peak of {peak}"
                    );
                }
            }
        }
    }
}
