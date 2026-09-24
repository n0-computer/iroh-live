# iroh-live-media-android

Android support for [`iroh-live-media`](../iroh-live-media): a camera bridge, an
EGL renderer, and JNI handle helpers. The [Android demo](../demos/android/) uses
it, and any Android Rust project can use it too.

Hardware H.264 through MediaCodec lives in `moq-video` behind
`cfg(target_os = "android")`, and backend selection picks it up on its own. The
`renderer` and `egl` modules only build on Android.

## `camera`

`camera(size, rate)` returns a `CameraSink` and a `VideoSource`. Pass the source
to `LocalBroadcast::set_video`. Kotlin pushes frames into the sink through JNI,
either as tightly packed RGBA with `push_rgba` or as a ready `moq_video::Frame`
with `push`.

A newer frame replaces one the encoder has not taken yet, because a stale camera
picture is worth less than the current one. `CameraSink::demand()` reports
whether any rendition is encoding, so the app can stop the camera while nobody
watches.

## `renderer`

`AndroidRenderer` owns the EGL context and draws to an `android.view.Surface`
with GLES2. `render_hardware_buffer` imports an `AHardwareBuffer` from
MediaCodec's `ImageReader` as a `GL_TEXTURE_EXTERNAL_OES` texture, with no copy.
`render_nv12` uploads the two planes of a software-decoded or preview frame and
converts them to RGB in the shader. Both letterbox the frame and apply the
sensor rotation.

## `egl`

Wrappers for the EGL and GLES extension functions the renderer needs, such as
`eglCreateImageKHR` and `glEGLImageTargetTexture2DOES`. They are not available
at link time, so the module resolves them at runtime through
`eglGetProcAddress`.

## `handle`

Passes an `Arc<Mutex<T>>` across JNI as a `jlong`. `to_i64` leaks a reference,
`from_i64` clones one, and `take_i64` takes it back.
