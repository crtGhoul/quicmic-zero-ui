package com.quicmic.webview;

import android.app.Activity;
import android.content.Intent;
import android.graphics.Color;
import android.hardware.Camera;
import android.os.Bundle;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.TextView;
import android.widget.Toast;

import com.google.zxing.BarcodeFormat;
import com.google.zxing.BinaryBitmap;
import com.google.zxing.DecodeHintType;
import com.google.zxing.MultiFormatReader;
import com.google.zxing.NotFoundException;
import com.google.zxing.PlanarYUVLuminanceSource;
import com.google.zxing.Result;
import com.google.zxing.common.HybridBinarizer;

import java.util.EnumMap;
import java.util.EnumSet;
import java.util.List;
import java.util.Map;

/**
 * Full-screen QR scanner used to read the pairing QR code from the PC app's
 * Pair tab. Uses the legacy {@link Camera} API plus ZXing core (bundled jar)
 * so the wrapper stays dependency-free and Gradle-free.
 */
public class ScanActivity extends Activity implements SurfaceHolder.Callback {

    public static final String EXTRA_RESULT = "scan_result";

    private Camera camera;
    private SurfaceView surfaceView;
    private final MultiFormatReader reader = new MultiFormatReader();
    private long lastAttemptMs;
    private boolean finished;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);

        surfaceView = new SurfaceView(this);
        surfaceView.setLayoutParams(new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));
        surfaceView.getHolder().addCallback(this);
        root.addView(surfaceView);

        ViewfinderView finder = new ViewfinderView(this);
        finder.setLayoutParams(new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));
        root.addView(finder);

        TextView hint = new TextView(this);
        hint.setText("Point at the QR code in the PC app's Pair tab");
        hint.setTextColor(Color.WHITE);
        hint.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15);
        hint.setGravity(Gravity.CENTER);
        FrameLayout.LayoutParams hlp = new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP);
        hlp.topMargin = dp(64);
        hint.setLayoutParams(hlp);
        root.addView(hint);

        Button cancel = new Button(this);
        cancel.setText("Cancel");
        cancel.setAllCaps(false);
        cancel.setTextColor(Color.WHITE);
        cancel.setBackgroundColor(0x88000000);
        FrameLayout.LayoutParams clp = new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM | Gravity.CENTER_HORIZONTAL);
        clp.bottomMargin = dp(48);
        cancel.setLayoutParams(clp);
        cancel.setOnClickListener(v -> finish());
        root.addView(cancel);

        setContentView(root);
    }

    private int dp(int v) {
        return Math.round(v * getResources().getDisplayMetrics().density);
    }

    @Override
    protected void onResume() {
        super.onResume();
        finished = false;
        openCamera();
    }

    @Override
    protected void onPause() {
        releaseCamera();
        super.onPause();
    }

    @SuppressWarnings("deprecation")
    private void openCamera() {
        try {
            camera = Camera.open();
        } catch (Exception e) {
            Toast.makeText(this, "Couldn't open the camera.", Toast.LENGTH_LONG).show();
            finish();
            return;
        }
        try {
            Camera.Parameters params = camera.getParameters();
            Camera.Size size = pickPreviewSize(params.getSupportedPreviewSizes());
            if (size != null) {
                params.setPreviewSize(size.width, size.height);
            }
            List<String> focusModes = params.getSupportedFocusModes();
            if (focusModes != null) {
                if (focusModes.contains(Camera.Parameters.FOCUS_MODE_CONTINUOUS_PICTURE)) {
                    params.setFocusMode(Camera.Parameters.FOCUS_MODE_CONTINUOUS_PICTURE);
                } else if (focusModes.contains(Camera.Parameters.FOCUS_MODE_AUTO)) {
                    params.setFocusMode(Camera.Parameters.FOCUS_MODE_AUTO);
                }
            }
            camera.setParameters(params);
            camera.setDisplayOrientation(90);
            if (surfaceView.getHolder().getSurface().isValid()) {
                camera.setPreviewDisplay(surfaceView.getHolder());
            }
            camera.setPreviewCallback(previewCallback);
            camera.startPreview();
        } catch (Exception e) {
            releaseCamera();
            Toast.makeText(this, "Couldn't start the camera preview.", Toast.LENGTH_LONG).show();
            finish();
        }
    }

    /** Largest preview at or under 720p: fast enough to decode in real time. */
    private static Camera.Size pickPreviewSize(List<Camera.Size> sizes) {
        if (sizes == null || sizes.isEmpty()) {
            return null;
        }
        Camera.Size best = null;
        for (Camera.Size s : sizes) {
            if (s.width <= 1280 && (best == null || s.width * s.height > best.width * best.height)) {
                best = s;
            }
        }
        return best != null ? best : sizes.get(0);
    }

    @SuppressWarnings("deprecation")
    private void releaseCamera() {
        if (camera != null) {
            try {
                camera.setPreviewCallback(null);
                camera.stopPreview();
                camera.release();
            } catch (Exception ignored) {
            }
            camera = null;
        }
    }

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
        // Preview display is attached in openCamera(); if the surface arrived
        // after the camera opened, attach it now.
        if (camera != null) {
            try {
                camera.setPreviewDisplay(holder);
            } catch (Exception ignored) {
            }
        }
    }

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int w, int h) {
    }

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {
    }

    private final Camera.PreviewCallback previewCallback = new Camera.PreviewCallback() {
        @Override
        public void onPreviewFrame(byte[] data, Camera cam) {
            if (finished || data == null) {
                return;
            }
            long now = System.currentTimeMillis();
            if (now - lastAttemptMs < 400) {
                return;
            }
            lastAttemptMs = now;
            Camera.Size size = cam.getParameters().getPreviewSize();
            String text = decodeQr(data, size.width, size.height);
            if (text != null) {
                finished = true;
                Intent result = new Intent();
                result.putExtra(EXTRA_RESULT, text);
                setResult(RESULT_OK, result);
                finish();
            }
        }
    };

    /**
     * Decodes a QR code from an NV21 preview frame. The sensor delivers
     * landscape frames; rotate 90° for the portrait display first.
     */
    private String decodeQr(byte[] data, int width, int height) {
        byte[] rotated = new byte[data.length];
        for (int y = 0; y < height; y++) {
            for (int x = 0; x < width; x++) {
                rotated[x * height + height - y - 1] = data[x + y * width];
            }
        }
        int tmp = width;
        width = height;
        height = tmp;
        try {
            PlanarYUVLuminanceSource source = new PlanarYUVLuminanceSource(
                    rotated, width, height, 0, 0, width, height, false);
            BinaryBitmap bitmap = new BinaryBitmap(new HybridBinarizer(source));
            Map<DecodeHintType, Object> hints = new EnumMap<>(DecodeHintType.class);
            hints.put(DecodeHintType.POSSIBLE_FORMATS, EnumSet.of(BarcodeFormat.QR_CODE));
            Result result = reader.decode(bitmap, hints);
            return result.getText();
        } catch (NotFoundException | IllegalArgumentException e) {
            return null;
        }
    }

    /** Dimmed overlay with a clear viewfinder square. */
    private static class ViewfinderView extends View {
        ViewfinderView(android.content.Context ctx) {
            super(ctx);
            setWillNotDraw(false);
        }

        @Override
        protected void onDraw(android.graphics.Canvas canvas) {
            super.onDraw(canvas);
            int w = getWidth();
            int h = getHeight();
            int side = (int) (Math.min(w, h) * 0.68);
            int left = (w - side) / 2;
            int top = (h - side) / 2;

            android.graphics.Paint dim = new android.graphics.Paint();
            dim.setColor(0xAA000000);
            canvas.drawRect(0, 0, w, top, dim);
            canvas.drawRect(0, top + side, w, h, dim);
            canvas.drawRect(0, top, left, top + side, dim);
            canvas.drawRect(left + side, top, w, top + side, dim);

            android.graphics.Paint border = new android.graphics.Paint();
            border.setColor(0xFF22D3EE);
            border.setStyle(android.graphics.Paint.Style.STROKE);
            border.setStrokeWidth(6);
            canvas.drawRect(left, top, left + side, top + side, border);
        }
    }
}
