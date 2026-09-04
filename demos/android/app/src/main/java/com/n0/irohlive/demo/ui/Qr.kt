package com.n0.irohlive.demo.ui

import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
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
            pixels[row + x] = if (matrix.get(x, y)) android.graphics.Color.BLACK
            else android.graphics.Color.WHITE
        }
    }
    return Bitmap.createBitmap(pixels, width, height, Bitmap.Config.ARGB_8888).asImageBitmap()
}

/**
 * A ticket as a QR code, with the text under it and a copy button.
 *
 * Shared by the publish and call screens: both put a code on screen for another
 * device's camera, and a code that renders differently in the two places would
 * be a code that reads differently.
 */
@Composable
fun TicketCard(ticket: String, onCopy: (String) -> Unit, modifier: Modifier = Modifier) {
    val qr = remember(ticket) { qrBitmap(ticket) }
    Card(
        shape = RoundedCornerShape(24.dp),
        colors = CardDefaults.cardColors(containerColor = Color.White),
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(20.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            if (qr != null) {
                Image(
                    bitmap = qr,
                    contentDescription = "Broadcast ticket",
                    // Nearest-neighbour keeps the module edges square when the
                    // bitmap is scaled up to the card width.
                    filterQuality = FilterQuality.None,
                    contentScale = ContentScale.Fit,
                    modifier = Modifier.fillMaxWidth().aspectRatio(1f),
                )
            } else {
                Text(
                    "This ticket is too long for a QR code.",
                    color = Color.Black,
                    textAlign = TextAlign.Center,
                )
            }
            Spacer(Modifier.height(12.dp))
            Text(
                text = ticket,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
                color = Color(0xFF444444),
                textAlign = TextAlign.Center,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Spacer(Modifier.height(8.dp))
            TextButton(onClick = { onCopy(ticket) }) { Text("Copy ticket") }
        }
    }
}
