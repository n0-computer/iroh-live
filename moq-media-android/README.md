# moq-media-android

Android integration for [`moq-media`](../moq-media): a camera bridge, an EGL
renderer, and the JNI helpers around them.

Hardware H.264 through MediaCodec is not here. It is upstream in `moq-video`,
behind `cfg(target_os = "android")`, and backend selection finds it without being
asked. What this crate carries is the two things that are not a moq-video
concern.

Used by the [Android demo](../demos/android/), and usable on its own in any
Android Rust project.

## `camera`

`camera(size)` returns a `CameraSink` and a `moq_media::publish::VideoSource`.
Kotlin pushes frames into the sink through JNI, and the publisher reads them out
the other end. `CameraSink::push_rgba` takes tightly packed RGBA; `push` takes a
`moq_video::Frame` for a caller that built one itself.

The slot is latest-wins: a newer frame replaces one the publisher has not read
yet. That is the right policy for a camera, where a stale picture is worth less
than the current one.

## `renderer`

`AndroidRenderer` owns the whole EGL lifecycle. Kotlin hands over an
`android.view.Surface` and nothing else. Two GLES2 programs draw:
`render_hardware_buffer` imports an `AHardwareBuffer` as a
`GL_TEXTURE_EXTERNAL_OES`, which is zero-copy out of MediaCodec's `ImageReader`,
and `render_nv12` uploads two planes for a software-decoded or preview frame.
Both apply sensor rotation in the shader.

## `egl`

Safe wrappers over the EGL and GLES extension entry points the renderer needs:
`eglGetNativeClientBufferANDROID`, `eglCreateImageKHR`, and
`glEGLImageTargetTexture2DOES`. None is available at link time, so they are
resolved at runtime through `dlopen` on `libEGL.so` and `eglGetProcAddress`.

## `handle`

Passing an `Arc<Mutex<T>>` across the JNI boundary as a `jlong`: `to_i64` leaks a
reference, `from_i64` borrows one, and `take_i64` consumes it.
