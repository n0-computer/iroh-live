package com.n0.irohlive.demo

import android.graphics.ImageFormat
import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.EGLContext
import android.opengl.EGLDisplay
import android.opengl.EGLSurface
import android.opengl.GLES11Ext
import android.opengl.GLES20
import android.os.Bundle
import android.util.Log
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer

/**
 * Main activity for the iroh-live Android demo.
 *
 * Supports two modes:
 * - **Watch**: subscribe-only viewer via a broadcast ticket.
 * - **Call**: dials a remote peer, publishes camera at 720p H.264 HW encoding
 *   and microphone audio (Opus), while subscribing to the remote's media.
 *
 * Video frames are decoded in Rust (zero-copy HW decoder) and rendered via
 * OpenGL ES external texture import from AHardwareBuffer.
 */
class MainActivity : AppCompatActivity() {
    companion object {
        private const val TAG = "IrohLiveDemo"
        private const val CAMERA_WIDTH = 1280
        private const val CAMERA_HEIGHT = 720

        // EGL extension constants not in the standard Android SDK.
        private const val EGL_NATIVE_BUFFER_ANDROID = 0x3140
        private const val EGL_IMAGE_PRESERVED_KHR = 0x30D2
        private const val EGL_NO_IMAGE_KHR = 0L

        // Vertex shader: pass-through with texture coordinates.
        private const val VERTEX_SHADER = """
            attribute vec4 aPosition;
            attribute vec2 aTexCoord;
            varying vec2 vTexCoord;
            void main() {
                gl_Position = aPosition;
                vTexCoord = aTexCoord;
            }
        """

        // Fragment shader: samples from an external OES texture.
        private const val FRAGMENT_SHADER = """
            #extension GL_OES_EGL_image_external : require
            precision mediump float;
            varying vec2 vTexCoord;
            uniform samplerExternalOES uTexture;
            void main() {
                gl_FragColor = texture2D(uTexture, vTexCoord);
            }
        """

        // Fullscreen quad vertices (two triangles) with tex coords.
        private val QUAD_VERTICES = floatArrayOf(
            // x, y, u, v
            -1f, -1f, 0f, 1f,
             1f, -1f, 1f, 1f,
            -1f,  1f, 0f, 0f,
             1f,  1f, 1f, 0f,
        )
    }

    private lateinit var ticketInput: EditText
    private lateinit var connectButton: Button
    private lateinit var dialButton: Button
    private lateinit var disconnectButton: Button
    private lateinit var videoSurface: SurfaceView
    private lateinit var cameraPreview: PreviewView
    private lateinit var statusText: TextView

    @Volatile
    private var sessionHandle: Long = 0
    private var renderJob: Job? = null
    private var cameraProvider: ProcessCameraProvider? = null
    private var surfaceReady = false

    // EGL state (initialized on render thread).
    private var eglDisplay: EGLDisplay = EGL14.EGL_NO_DISPLAY
    private var eglContext: EGLContext = EGL14.EGL_NO_CONTEXT
    private var eglSurface: EGLSurface = EGL14.EGL_NO_SURFACE
    private var glProgram = 0
    private var glTexture = 0
    private var vertexBuffer: FloatBuffer? = null

    // Permission launcher for camera and microphone.
    private val permissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { grants ->
        val allGranted = grants.values.all { it }
        if (!allGranted) {
            Log.w(TAG, "Not all permissions were granted: $grants")
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        ticketInput = findViewById(R.id.ticketInput)
        connectButton = findViewById(R.id.connectButton)
        dialButton = findViewById(R.id.dialButton)
        disconnectButton = findViewById(R.id.disconnectButton)
        videoSurface = findViewById(R.id.videoSurface)
        cameraPreview = findViewById(R.id.cameraPreview)
        statusText = findViewById(R.id.statusText)

        permissionLauncher.launch(
            arrayOf(
                android.Manifest.permission.CAMERA,
                android.Manifest.permission.RECORD_AUDIO,
            )
        )

        videoSurface.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) {
                surfaceReady = true
            }

            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
                surfaceWidth = width
                surfaceHeight = height
            }

            override fun surfaceDestroyed(holder: SurfaceHolder) {
                surfaceReady = false
            }
        })

        connectButton.setOnClickListener { onConnect() }
        dialButton.setOnClickListener { onDial() }
        disconnectButton.setOnClickListener { onDisconnect() }
    }

    override fun onDestroy() {
        super.onDestroy()
        if (sessionHandle != 0L) {
            IrohBridge.disconnect(sessionHandle)
            sessionHandle = 0
        }
    }

    private fun onConnect() {
        val ticket = ticketInput.text.toString().trim()
        if (ticket.isEmpty()) {
            statusText.text = "Enter a ticket string"
            return
        }

        connectButton.isEnabled = false
        dialButton.isEnabled = false
        statusText.text = "Connecting..."

        lifecycleScope.launch {
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.connect(ticket)
            }

            if (handle == 0L) {
                statusText.text = "Connection failed"
                connectButton.isEnabled = true
                dialButton.isEnabled = true
                return@launch
            }

            sessionHandle = handle
            statusText.text = "Connected"
            disconnectButton.isEnabled = true

            startRenderLoop()
        }
    }

    private fun onDial() {
        val ticket = ticketInput.text.toString().trim()
        if (ticket.isEmpty()) {
            statusText.text = "Enter a call ticket"
            return
        }

        connectButton.isEnabled = false
        dialButton.isEnabled = false
        statusText.text = "Dialing..."

        // Start camera first so frames are ready when the encoder starts.
        startCamera()

        lifecycleScope.launch {
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.dial(ticket, CAMERA_WIDTH, CAMERA_HEIGHT)
            }

            if (handle == 0L) {
                statusText.text = "Call failed"
                connectButton.isEnabled = true
                dialButton.isEnabled = true
                stopCamera()
                return@launch
            }

            sessionHandle = handle
            statusText.text = "In call"
            disconnectButton.isEnabled = true

            // Start pushing camera frames to the Rust encoder.
            startCameraFramePush()

            startRenderLoop()
        }
    }

    // ── Camera (CameraX) ────────────────────────────────────────────────

    private var imageAnalysis: ImageAnalysis? = null

    private fun startCamera() {
        val cameraProviderFuture = ProcessCameraProvider.getInstance(this)
        cameraProviderFuture.addListener({
            val provider = cameraProviderFuture.get()
            cameraProvider = provider

            val resolutionSelector = ResolutionSelector.Builder()
                .setResolutionStrategy(
                    ResolutionStrategy(
                        android.util.Size(CAMERA_WIDTH, CAMERA_HEIGHT),
                        ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER
                    )
                )
                .build()

            val preview = Preview.Builder()
                .setResolutionSelector(resolutionSelector)
                .build()
                .also { it.surfaceProvider = cameraPreview.surfaceProvider }

            val analysis = ImageAnalysis.Builder()
                .setResolutionSelector(resolutionSelector)
                .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()
            imageAnalysis = analysis

            try {
                provider.unbindAll()
                provider.bindToLifecycle(
                    this, CameraSelector.DEFAULT_FRONT_CAMERA, preview, analysis
                )
                cameraPreview.visibility = View.VISIBLE
                Log.i(TAG, "CameraX started")
            } catch (e: Exception) {
                Log.e(TAG, "CameraX bind failed", e)
            }
        }, ContextCompat.getMainExecutor(this))
    }

    private fun stopCamera() {
        cameraProvider?.unbindAll()
        cameraProvider = null
        imageAnalysis = null
        cameraPreview.visibility = View.GONE
    }

    /**
     * Pushes NV12 plane data from the ImageAnalysis pipeline directly to Rust.
     *
     * CameraX YUV_420_888 on Android typically has UV pixel stride 2, which is
     * NV12 (interleaved VU). We extract Y and UV planes and pass them straight
     * through — no CPU colorspace conversion needed.
     *
     * If the UV pixel stride is 1 (planar I420, rare on modern devices), we
     * interleave U+V into NV12 layout as a fallback.
     */
    private var nv12FrameCount = 0L

    private fun startCameraFramePush() {
        val analysis = imageAnalysis ?: return
        nv12FrameCount = 0
        val executor = java.util.concurrent.Executors.newSingleThreadExecutor()
        analysis.setAnalyzer(executor) { image ->
            val handle = sessionHandle
            if (handle != 0L && image.format == ImageFormat.YUV_420_888) {
                pushNv12(image, handle)
                nv12FrameCount++
                if (nv12FrameCount == 1L) {
                    Log.i(TAG, "First NV12 frame pushed: ${image.width}x${image.height} " +
                        "uvPixelStride=${image.planes[1].pixelStride} " +
                        "yStride=${image.planes[0].rowStride} " +
                        "uvStride=${image.planes[1].rowStride}")
                }
            }
            image.close()
        }
    }

    private fun pushNv12(image: ImageProxy, handle: Long) {
        val yPlane = image.planes[0]
        val uvPlane = image.planes[1] // U plane; on NV12 devices, interleaved UV
        val vPlane = image.planes[2]

        val width = image.width
        val height = image.height
        val yStride = yPlane.rowStride
        val uvStride = uvPlane.rowStride
        val uvPixelStride = uvPlane.pixelStride

        // Extract Y plane bytes (may include row padding).
        val yBuf = yPlane.buffer
        val ySize = yStride * height
        val yData = ByteArray(ySize)
        yBuf.position(0)
        yBuf.get(yData, 0, ySize.coerceAtMost(yBuf.remaining()))

        val uvHeight = height / 2

        if (uvPixelStride == 2) {
            // NV12/NV21: UV data is already interleaved. The U plane buffer
            // on Android starts at the first U byte; V plane starts 1 byte
            // earlier (NV21=VU) or 1 byte later (NV12=UV). We use the V plane
            // which starts at the V byte — for NV21 this gives us VUVU... which
            // the encoder expects as interleaved chroma. We read from whichever
            // plane starts first to get the full interleaved buffer.
            val uvBuf = vPlane.buffer // V plane on Android NV21 starts at V byte
            val uvSize = uvStride * uvHeight
            val uvData = ByteArray(uvSize)
            uvBuf.position(0)
            uvBuf.get(uvData, 0, uvSize.coerceAtMost(uvBuf.remaining()))

            IrohBridge.pushCameraNv12(handle, yData, uvData, width, height, yStride, uvStride)
        } else {
            // Planar I420 (pixel stride 1): manually interleave U+V into NV12.
            val uBuf = uvPlane.buffer
            val vBuf = vPlane.buffer
            val uvWidth = width / 2
            val uvData = ByteArray(uvStride * uvHeight)
            for (row in 0 until uvHeight) {
                for (col in 0 until uvWidth) {
                    val srcIdx = row * uvPlane.rowStride + col
                    val dstIdx = row * uvStride + col * 2
                    uvData[dstIdx] = uBuf.get(srcIdx)
                    uvData[dstIdx + 1] = vBuf.get(srcIdx)
                }
            }
            IrohBridge.pushCameraNv12(handle, yData, uvData, width, height, yStride, width)
        }
    }

    private fun onDisconnect() {
        val handle = sessionHandle
        sessionHandle = 0

        val job = renderJob
        renderJob = null

        stopCamera()

        disconnectButton.isEnabled = false
        connectButton.isEnabled = true
        dialButton.isEnabled = true
        statusText.text = "Disconnected"

        if (handle != 0L) {
            lifecycleScope.launch(Dispatchers.IO) {
                job?.cancelAndJoin()
                IrohBridge.disconnect(handle)
            }
        }
    }

    // Current surface dimensions (updated from surfaceChanged).
    private var surfaceWidth = 0
    private var surfaceHeight = 0

    /**
     * Render loop: acquires HardwareBuffer frames from the Rust decoder,
     * imports them as GL OES textures, and renders a fullscreen quad with
     * correct aspect ratio (letterboxed).
     */
    private fun startRenderLoop() {
        renderJob = lifecycleScope.launch(Dispatchers.Default) {
            // Wait for surface to be ready.
            while (isActive && !surfaceReady) {
                delay(50L)
            }
            if (!isActive) return@launch

            val holder = videoSurface.holder

            try {
                initEgl(holder)
                initGl()
            } catch (e: Exception) {
                Log.e(TAG, "EGL/GL init failed", e)
                return@launch
            }

            var statusCounter = 0L
            try {
                while (isActive) {
                    val handle = sessionHandle
                    if (handle == 0L) break

                    if (!surfaceReady) {
                        delay(50L)
                        continue
                    }

                    // Update status line every ~30 frames (~1s at 30fps).
                    statusCounter++
                    if (statusCounter % 30 == 0L) {
                        val line = IrohBridge.getStatusLine(handle)
                        if (line.isNotEmpty()) {
                            runOnUiThread { statusText.text = line }
                        }
                    }

                    val bufferPtr = IrohBridge.nextHardwareBuffer(handle)
                    if (bufferPtr == 0L) {
                        delay(2L)
                        continue
                    }

                    try {
                        renderHardwareBuffer(handle, bufferPtr)
                    } finally {
                        IrohBridge.releaseHardwareBuffer(bufferPtr)
                    }
                }
            } finally {
                teardownEgl()
            }
        }
    }

    /**
     * Imports the AHardwareBuffer as an EGLImage, binds it to the OES
     * texture, and renders an aspect-correct letterboxed quad.
     */
    private fun renderHardwareBuffer(sessionHandle: Long, bufferPtr: Long) {
        // Get EGLClientBuffer from AHardwareBuffer.
        val clientBuffer = eglGetNativeClientBufferANDROID(bufferPtr)
        if (clientBuffer == 0L) {
            Log.w(TAG, "eglGetNativeClientBufferANDROID returned null")
            return
        }

        // Create EGLImage from the client buffer.
        val attrs = intArrayOf(EGL_IMAGE_PRESERVED_KHR, EGL14.EGL_TRUE, EGL14.EGL_NONE)
        val eglImage = eglCreateImageKHR(
            eglDisplay, EGL14.EGL_NO_CONTEXT,
            EGL_NATIVE_BUFFER_ANDROID, clientBuffer, attrs
        )
        if (eglImage == EGL_NO_IMAGE_KHR) {
            Log.w(TAG, "eglCreateImageKHR failed")
            return
        }

        try {
            // Bind EGLImage to the OES texture.
            GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
            GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, glTexture)
            glEGLImageTargetTexture2DOES(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, eglImage)

            // Compute aspect-correct viewport (letterbox).
            val sw = surfaceWidth
            val sh = surfaceHeight
            val dims = IrohBridge.getVideoDimensions(sessionHandle)
            val vw = (dims ushr 32).toInt()
            val vh = (dims and 0xFFFFFFFFL).toInt()

            // Clear full surface to black (letterbox bars).
            GLES20.glViewport(0, 0, sw.coerceAtLeast(1), sh.coerceAtLeast(1))
            GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT)

            // Set aspect-correct viewport for the video quad.
            if (sw > 0 && sh > 0 && vw > 0 && vh > 0) {
                val videoAspect = vw.toFloat() / vh.toFloat()
                val surfaceAspect = sw.toFloat() / sh.toFloat()
                val vpX: Int
                val vpY: Int
                val vpW: Int
                val vpH: Int
                if (videoAspect > surfaceAspect) {
                    // Video is wider — pillarbox top/bottom.
                    vpW = sw
                    vpH = (sw / videoAspect).toInt()
                    vpX = 0
                    vpY = (sh - vpH) / 2
                } else {
                    // Video is taller — letterbox left/right.
                    vpH = sh
                    vpW = (sh * videoAspect).toInt()
                    vpX = (sw - vpW) / 2
                    vpY = 0
                }
                GLES20.glViewport(vpX, vpY, vpW, vpH)
            }
            GLES20.glUseProgram(glProgram)

            val posLoc = GLES20.glGetAttribLocation(glProgram, "aPosition")
            val texLoc = GLES20.glGetAttribLocation(glProgram, "aTexCoord")
            val samplerLoc = GLES20.glGetUniformLocation(glProgram, "uTexture")

            GLES20.glUniform1i(samplerLoc, 0)

            vertexBuffer?.let { vb ->
                vb.position(0)
                GLES20.glEnableVertexAttribArray(posLoc)
                GLES20.glVertexAttribPointer(posLoc, 2, GLES20.GL_FLOAT, false, 16, vb)

                vb.position(2)
                GLES20.glEnableVertexAttribArray(texLoc)
                GLES20.glVertexAttribPointer(texLoc, 2, GLES20.GL_FLOAT, false, 16, vb)

                GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)

                GLES20.glDisableVertexAttribArray(posLoc)
                GLES20.glDisableVertexAttribArray(texLoc)
            }

            EGL14.eglSwapBuffers(eglDisplay, eglSurface)
        } finally {
            eglDestroyImageKHR(eglDisplay, eglImage)
        }
    }

    // ── EGL/GL setup ────────────────────────────────────────────────────

    private fun initEgl(holder: SurfaceHolder) {
        eglDisplay = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        check(eglDisplay != EGL14.EGL_NO_DISPLAY) { "eglGetDisplay failed" }

        val version = IntArray(2)
        check(EGL14.eglInitialize(eglDisplay, version, 0, version, 1)) { "eglInitialize failed" }

        val configAttribs = intArrayOf(
            EGL14.EGL_RED_SIZE, 8,
            EGL14.EGL_GREEN_SIZE, 8,
            EGL14.EGL_BLUE_SIZE, 8,
            EGL14.EGL_ALPHA_SIZE, 8,
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
            EGL14.EGL_SURFACE_TYPE, EGL14.EGL_WINDOW_BIT,
            EGL14.EGL_NONE,
        )
        val configs = arrayOfNulls<EGLConfig>(1)
        val numConfigs = IntArray(1)
        check(
            EGL14.eglChooseConfig(eglDisplay, configAttribs, 0, configs, 0, 1, numConfigs, 0)
            && numConfigs[0] > 0
        ) { "eglChooseConfig failed" }

        val contextAttribs = intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE)
        eglContext = EGL14.eglCreateContext(
            eglDisplay, configs[0]!!, EGL14.EGL_NO_CONTEXT, contextAttribs, 0
        )
        check(eglContext != EGL14.EGL_NO_CONTEXT) { "eglCreateContext failed" }

        val surfaceAttribs = intArrayOf(EGL14.EGL_NONE)
        eglSurface = EGL14.eglCreateWindowSurface(
            eglDisplay, configs[0]!!, holder.surface, surfaceAttribs, 0
        )
        check(eglSurface != EGL14.EGL_NO_SURFACE) { "eglCreateWindowSurface failed" }

        check(
            EGL14.eglMakeCurrent(eglDisplay, eglSurface, eglSurface, eglContext)
        ) { "eglMakeCurrent failed" }

        Log.i(TAG, "EGL initialized: ${EGL14.eglQueryString(eglDisplay, EGL14.EGL_VERSION)}")
    }

    private fun initGl() {
        // Create shader program.
        val vs = compileShader(GLES20.GL_VERTEX_SHADER, VERTEX_SHADER)
        val fs = compileShader(GLES20.GL_FRAGMENT_SHADER, FRAGMENT_SHADER)
        glProgram = GLES20.glCreateProgram()
        GLES20.glAttachShader(glProgram, vs)
        GLES20.glAttachShader(glProgram, fs)
        GLES20.glLinkProgram(glProgram)
        GLES20.glDeleteShader(vs)
        GLES20.glDeleteShader(fs)

        val linkStatus = IntArray(1)
        GLES20.glGetProgramiv(glProgram, GLES20.GL_LINK_STATUS, linkStatus, 0)
        check(linkStatus[0] == GLES20.GL_TRUE) {
            val log = GLES20.glGetProgramInfoLog(glProgram)
            GLES20.glDeleteProgram(glProgram)
            "Shader link failed: $log"
        }

        // Create OES texture.
        val texIds = IntArray(1)
        GLES20.glGenTextures(1, texIds, 0)
        glTexture = texIds[0]
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, glTexture)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE)

        // Vertex buffer.
        val bb = ByteBuffer.allocateDirect(QUAD_VERTICES.size * 4)
        bb.order(ByteOrder.nativeOrder())
        vertexBuffer = bb.asFloatBuffer().apply {
            put(QUAD_VERTICES)
            position(0)
        }

        GLES20.glClearColor(0f, 0f, 0f, 1f)

        Log.i(TAG, "GL initialized: renderer=${GLES20.glGetString(GLES20.GL_RENDERER)}")
    }

    private fun compileShader(type: Int, source: String): Int {
        val shader = GLES20.glCreateShader(type)
        GLES20.glShaderSource(shader, source)
        GLES20.glCompileShader(shader)
        val status = IntArray(1)
        GLES20.glGetShaderiv(shader, GLES20.GL_COMPILE_STATUS, status, 0)
        check(status[0] == GLES20.GL_TRUE) {
            val log = GLES20.glGetShaderInfoLog(shader)
            GLES20.glDeleteShader(shader)
            "Shader compile failed: $log"
        }
        return shader
    }

    private fun teardownEgl() {
        if (eglDisplay != EGL14.EGL_NO_DISPLAY) {
            EGL14.eglMakeCurrent(eglDisplay, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)
            if (eglSurface != EGL14.EGL_NO_SURFACE) {
                EGL14.eglDestroySurface(eglDisplay, eglSurface)
                eglSurface = EGL14.EGL_NO_SURFACE
            }
            if (eglContext != EGL14.EGL_NO_CONTEXT) {
                EGL14.eglDestroyContext(eglDisplay, eglContext)
                eglContext = EGL14.EGL_NO_CONTEXT
            }
            EGL14.eglTerminate(eglDisplay)
            eglDisplay = EGL14.EGL_NO_DISPLAY
        }
        if (glTexture != 0) {
            // Texture is invalid after context destruction, just zero it.
            glTexture = 0
        }
        glProgram = 0
        vertexBuffer = null

        Log.i(TAG, "EGL teardown complete")
    }

    // ── Native EGL extension functions ──────────────────────────────────
    //
    // These are JNI wrappers around EGL/GLES extension functions that are
    // not exposed in the Android Java SDK. They call the NDK C functions
    // via dlsym at runtime.

    private external fun eglGetNativeClientBufferANDROID(hardwareBufferPtr: Long): Long
    private external fun eglCreateImageKHR(
        display: EGLDisplay, context: EGLContext,
        target: Int, clientBuffer: Long, attrs: IntArray
    ): Long
    private external fun eglDestroyImageKHR(display: EGLDisplay, image: Long)
    private external fun glEGLImageTargetTexture2DOES(target: Int, image: Long)
}
