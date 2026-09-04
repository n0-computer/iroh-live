package com.n0.irohlive.demo.ui

import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import com.n0.irohlive.demo.RenderTarget

/**
 * The surface the native renderer draws into.
 *
 * A [SurfaceView] rather than a `TextureView`: the decoder hands GL an
 * `AHardwareBuffer` and the compositor can scan a surface out without a
 * detour through the view hierarchy. The aspect ratio is handled in the
 * renderer, which letterboxes inside whatever size this view reports, so the
 * view itself can simply fill its parent.
 */
@Composable
fun VideoSurface(target: RenderTarget, modifier: Modifier = Modifier) {
    AndroidView(
        modifier = modifier,
        factory = { context ->
            SurfaceView(context).apply {
                holder.addCallback(object : SurfaceHolder.Callback {
                    override fun surfaceCreated(holder: SurfaceHolder) {
                        target.holder = holder
                    }

                    override fun surfaceChanged(
                        holder: SurfaceHolder,
                        format: Int,
                        width: Int,
                        height: Int,
                    ) {
                        target.width = width
                        target.height = height
                        target.holder = holder
                    }

                    override fun surfaceDestroyed(holder: SurfaceHolder) {
                        target.holder = null
                    }
                })
            }
        },
        onRelease = { target.holder = null },
    )
}
