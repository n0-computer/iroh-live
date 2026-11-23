//! A video source that scales another source's frames to I420 as they arrive.

use std::{sync::Arc, time::Instant};

use n0_future::task::AbortOnDropHandle;
use tracing::{Instrument, debug, debug_span, warn};

use super::{Demand, Driver, VideoFormat, VideoSource};
use crate::{Error, frames::FrameSlot, video};

impl VideoSource {
    /// Returns a source of this source's frames scaled to `size`, as I420 in CPU memory.
    ///
    /// The output is always CPU I420, so this is not meant for publishing:
    /// a GPU frame would be downloaded and uploaded again for a hardware
    /// encoder. Use `VideoRendition::with_size` in `moq-ext-publisher`
    /// instead, which keeps GPU frames on the GPU.
    ///
    /// GPU frames are scaled on their device where supported. The picture is
    /// stretched to exactly `size`, and only the newest frame is scaled.
    ///
    /// A frame that can never be scaled ends the source, even over a
    /// [`switchable`](Self::switchable) input that later moves on, and
    /// [`failure`](Self::failure) returns why.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] if `size` is zero or odd.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn scaled(&self, size: video::Size) -> Result<Self, Error> {
        if size.width == 0
            || size.height == 0
            || !size.width.is_multiple_of(2)
            || !size.height.is_multiple_of(2)
        {
            return Err(Error::invalid(format!(
                "cannot scale to {size}: I420 needs an even, non-zero size"
            )));
        }
        let slot = FrameSlot::new();
        let reader = slot.reader();
        let demand = Demand::default();
        let format = VideoFormat {
            size,
            rate: self.format().rate,
        };
        let span = debug_span!("scaled", input = self.label(), %size);
        let task = AbortOnDropHandle::new(n0_future::task::spawn(
            run(self.clone(), size, slot, demand.clone()).instrument(span),
        ));
        let driver = Driver::Derived {
            input: self.clone(),
            _task: task,
        };
        Ok(Self::with_demand(
            format!("{}-scaled", self.label()),
            format,
            reader,
            Default::default(),
            driver,
            demand,
        ))
    }
}

/// Scales the frames of `input` to `size` into `slot` while `demand` wants them.
///
/// Returns once `input` ends, or once a frame can never be scaled. Records
/// the input's failure, or its own, in `slot`.
async fn run(input: VideoSource, size: video::Size, slot: FrameSlot, demand: Demand) {
    // Observed, so the input is wanted only while the scaled source is.
    let mut frames = input.observe();
    let mut wanted = demand.watch();
    let mut guard = None;
    // Whether the last frame failed to scale, so a run of failures warns once.
    let mut failing = false;
    // A keyframe request on the input that no scaled frame has carried yet.
    let mut keyframe = false;
    loop {
        let want = *wanted.borrow_and_update();
        if want != guard.is_some() {
            guard = want.then(|| input.want());
        }
        // The demand cannot close: this task holds it.
        let frame = if guard.is_some() {
            tokio::select! {
                frame = frames.next() => frame,
                _ = wanted.changed() => continue,
            }
        } else {
            tokio::select! {
                () = frames.closed() => None,
                _ = wanted.changed() => continue,
            }
        };
        let Some(frame) = frame else { break };
        let at = frames.sent_at().unwrap_or_else(Instant::now);
        keyframe |= frames.keyframe_requested();
        match tokio::task::spawn_blocking(move || to_i420(frame, size)).await {
            Ok(Ok(frame)) => {
                failing = false;
                if std::mem::take(&mut keyframe) {
                    slot.request_keyframe();
                }
                slot.send_at(frame, at);
            }
            Ok(Err(err)) if is_permanent(&err) => {
                warn!(error = format_args!("{err:#}"), "cannot scale this input");
                slot.fail(Arc::new(Error::device(err)));
                return;
            }
            Ok(Err(err)) if !failing => {
                failing = true;
                warn!(
                    error = format_args!("{err:#}"),
                    "cannot scale a frame, skipping frames until one scales"
                );
            }
            Ok(Err(err)) => debug!(error = format_args!("{err:#}"), "cannot scale a frame"),
            Err(err) => {
                let err = Error::device_msg(format!("the scaling task failed: {err}"));
                slot.fail(Arc::new(err));
                return;
            }
        }
    }
    if let Some(failure) = input.failure() {
        slot.fail(failure);
    }
}

/// Returns whether `err` fails every later frame of the same input too.
///
/// An input delivers one kind of surface, so a surface this build or platform
/// cannot scale at all will not scale on the next frame either.
fn is_permanent(err: &video::Error) -> bool {
    matches!(err, video::Error::Unsupported(_))
}

/// Returns `frame` scaled to `size` in I420, or `frame` itself if it already is.
fn to_i420(frame: Arc<video::Frame>, size: video::Size) -> Result<Arc<video::Frame>, video::Error> {
    if frame.size() == size && matches!(frame.surface, video::Surface::I420(_)) {
        return Ok(frame);
    }
    let image = if frame.size() == size {
        frame.surface.to_i420()?.into_owned()
    } else {
        // A GPU surface scales on its device, and only the small picture is
        // downloaded.
        frame
            .surface
            .resize(size, &Default::default())?
            .into_i420()?
    };
    Ok(Arc::new(video::Frame::new(
        video::Surface::I420(image),
        frame.timestamp,
    )))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::FrameSender;

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn format(size: video::Size) -> VideoFormat {
        VideoFormat {
            size,
            rate: video::FrameRate::new(30, 1).expect("a valid rate"),
        }
    }

    fn push_rgba(sender: &FrameSender<video::Frame>, size: video::Size, micros: u64) {
        let len = (size.width * size.height * 4) as usize;
        let surface = video::Surface::rgba(&vec![128; len], size).expect("a valid size");
        let timestamp = moq_net::Timestamp::from_micros(micros).expect("in range");
        sender
            .push(video::Frame::new(surface, timestamp))
            .expect("open");
    }

    async fn next(frames: &mut crate::VideoFrames) -> Arc<video::Frame> {
        tokio::time::timeout(TIMEOUT, frames.next())
            .await
            .expect("a frame in time")
            .expect("the stream is open")
    }

    #[tokio::test]
    async fn frames_arrive_scaled_in_i420_with_their_timestamp() {
        let big = video::Size::new(8, 6);
        let small = video::Size::new(4, 2);
        let (sender, source) = VideoSource::push(format(big));
        let scaled = source.scaled(small).expect("a valid size");
        assert_eq!(scaled.format().size, small);
        assert_eq!(scaled.format().rate, source.format().rate);
        let mut frames = scaled.frames();
        push_rgba(&sender, big, 42);
        let frame = next(&mut frames).await;
        assert_eq!(frame.size(), small);
        assert!(matches!(frame.surface, video::Surface::I420(_)));
        assert_eq!(frame.timestamp.as_micros(), Ok(42));
    }

    #[tokio::test]
    async fn a_frame_of_the_right_size_in_i420_passes_through() {
        let size = video::Size::new(4, 2);
        let (sender, source) = VideoSource::push(format(size));
        let scaled = source.scaled(size).expect("a valid size");
        let mut input = source.observe();
        let mut frames = scaled.frames();
        let rgba = video::Surface::rgba(&[128; 32], size).expect("4x2");
        let image = rgba.to_i420().expect("a CPU picture").into_owned();
        let timestamp = moq_net::Timestamp::from_micros(2).expect("in range");
        sender
            .push(video::Frame::new(video::Surface::I420(image), timestamp))
            .expect("open");
        let pushed = next(&mut input).await;
        assert!(Arc::ptr_eq(&pushed, &next(&mut frames).await));
    }

    #[test]
    fn only_an_unsupported_surface_fails_the_source() {
        assert!(is_permanent(&video::Error::Unsupported(
            "an external Vulkan surface".into()
        )));
        assert!(!is_permanent(&video::Error::CodecGone("reset".into())));
    }

    #[tokio::test]
    async fn the_label_names_the_input() {
        let (_sender, source) = VideoSource::push(format(video::Size::new(4, 4)));
        let scaled = source.scaled(video::Size::new(2, 2)).expect("a valid size");
        assert_eq!(scaled.label(), format!("{}-scaled", source.label()));
    }

    #[test]
    fn an_odd_or_empty_size_is_refused() {
        let (_sender, source) = VideoSource::push(format(video::Size::new(4, 4)));
        for size in [video::Size::new(3, 2), video::Size::new(0, 2)] {
            assert!(matches!(
                source.scaled(size),
                Err(Error::InvalidConfig { .. })
            ));
        }
    }

    #[tokio::test]
    async fn the_input_is_wanted_only_while_the_scaled_source_is() {
        let size = video::Size::new(4, 4);
        let (sender, source) = VideoSource::push(format(size));
        let scaled = source.scaled(video::Size::new(2, 2)).expect("a valid size");
        let mut demand = sender.demand();
        assert!(!*demand.borrow());
        let frames = scaled.frames();
        tokio::time::timeout(TIMEOUT, demand.wait_for(|wanted| *wanted))
            .await
            .expect("the input became wanted")
            .expect("the input is open");
        drop(frames);
        tokio::time::timeout(TIMEOUT, demand.wait_for(|wanted| !*wanted))
            .await
            .expect("the input stopped being wanted")
            .expect("the input is open");
    }

    /// The scaled source keeps its input running, and lets go of it once dropped.
    #[tokio::test]
    async fn the_scaled_source_owns_its_input() {
        let size = video::Size::new(4, 4);
        let (sender, source) = VideoSource::push(format(size));
        let scaled = source.scaled(video::Size::new(2, 2)).expect("a valid size");
        drop(source);
        assert!(
            !sender.is_closed(),
            "the input closed under the scaled source"
        );
        let mut frames = scaled.frames();
        push_rgba(&sender, size, 3);
        assert_eq!(next(&mut frames).await.timestamp.as_micros(), Ok(3));

        drop(scaled);
        tokio::time::timeout(TIMEOUT, sender.closed())
            .await
            .expect("the input closed with the scaled source");
        assert!(
            tokio::time::timeout(TIMEOUT, frames.next())
                .await
                .expect("the stream ended")
                .is_none(),
            "a frames handle outlived its source"
        );
    }

    #[tokio::test]
    async fn the_scaled_source_ends_with_its_input() {
        let size = video::Size::new(4, 4);
        let (sender, source) = VideoSource::push(format(size));
        let scaled = source.scaled(video::Size::new(2, 2)).expect("a valid size");
        drop(source);
        let mut frames = scaled.frames();
        push_rgba(&sender, size, 5);
        assert_eq!(next(&mut frames).await.timestamp.as_micros(), Ok(5));
        drop(sender);
        assert!(
            tokio::time::timeout(TIMEOUT, frames.next())
                .await
                .expect("the stream ended")
                .is_none()
        );
        assert!(scaled.failure().is_none());
    }

    #[tokio::test]
    async fn the_scaled_source_reports_the_failure_of_its_input() {
        let size = video::Size::new(4, 4);
        let source = VideoSource::spawn("failing", format(size), |_sender| {
            Err(Error::device_msg("the screen went away"))
        })
        .expect("the thread starts");
        let scaled = source.scaled(video::Size::new(2, 2)).expect("a valid size");
        let mut frames = scaled.frames();
        assert!(
            tokio::time::timeout(TIMEOUT, frames.next())
                .await
                .expect("the stream ended")
                .is_none()
        );
        assert!(matches!(
            scaled.failure().as_deref(),
            Some(Error::Device { .. })
        ));
    }
}
