# iroh-live-media-android

Android support for [`iroh-live-media`](../iroh-live-media): an EGL renderer and
JNI handle helpers. The [Android demo](../demos/android/) uses
it, and any Android Rust project can use it too.

Hardware H.264 through MediaCodec lives in `moq-video` behind
`cfg(target_os = "android")`, and backend selection picks it up on its own. The
`renderer` module only builds on Android.

Camera frames need no bridge: `iroh_live_media::VideoSource::push` returns a
`FrameSender` that Kotlin's frame callbacks push into through JNI.

## `renderer`

`AndroidRenderer` owns the EGL context and draws to an `android.view.Surface`
with GLES2. `render_hardware_buffer` imports an `AHardwareBuffer` from
MediaCodec's `ImageReader` as a `GL_TEXTURE_EXTERNAL_OES` texture, with no copy.
`render_nv12` uploads the two planes of a software-decoded or preview frame and
converts them to RGB in the shader. Both letterbox the frame and apply the
sensor rotation.

## `handle`

Passes an `Arc<Mutex<T>>` across JNI as a `jlong`. `to_i64` leaks a reference,
`from_i64` clones one, and `take_i64` takes it back.
