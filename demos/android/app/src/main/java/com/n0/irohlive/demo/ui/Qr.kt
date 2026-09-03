package com.n0.irohlive.demo.ui

import android.graphics.Bitmap
import android.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel

/**
 * Encodes [text] as a QR code bitmap [size] pixels on a side.
 *
 * The error correction level is the lowest one, because a ticket carries an
 * endpoint id and a broadcast name and every redundancy level above `L` costs
 * modules that a phone camera then has to resolve. Returns null when the text
 * is too long for any QR version.
 */
fun qrBitmap(text: String, size: Int = 720): ImageBitmap? {
    if (text.isEmpty()) return null
    val hints = mapOf(
        EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.L,
        EncodeHintType.MARGIN to 1,
        EncodeHintType.CHARACTER_SET to "UTF-8",
    )
    val matrix = try {
        QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, size, size, hints)
    } catch (e: Exception) {
        return null
    }
    val width = matrix.width
    val height = matrix.height
    val pixels = IntArray(width * height)
    for (y in 0 until height) {
        val row = y * width
        for (x in 0 until width) {
            pixels[row + x] = if (matrix.get(x, y)) Color.BLACK else Color.WHITE
        }
    }
    return Bitmap.createBitmap(pixels, width, height, Bitmap.Config.ARGB_8888).asImageBitmap()
}
