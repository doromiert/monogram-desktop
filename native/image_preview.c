// SPDX-License-Identifier: GPL-3.0-or-later
// Worker-only first-frame extraction from an already bounded, private local prefix.
#include <gtk/gtk.h>
#include <string.h>
#include <gst/gst.h>
#include <gst/app/gstappsink.h>
#include <gst/video/video.h>

static gint preview_autoplug(GstElement *decode, GstPad *pad, GstCaps *caps,
                            GstElementFactory *factory, gpointer unused) {
    (void)decode; (void)pad; (void)unused;
    if (gst_caps_get_size(caps)) {
        const GstStructure *format = gst_caps_get_structure(caps, 0);
        gint width, height;
        if (gst_structure_get_int(format, "width", &width) && gst_structure_get_int(format, "height", &height)
            && (width <= 0 || height <= 0 || (guint64)width * height > 40000000)) return 2;
    }
    const gchar *name = gst_plugin_feature_get_name(GST_PLUGIN_FEATURE(factory));
    const gchar *kind = gst_element_factory_get_metadata(factory, GST_ELEMENT_METADATA_KLASS);
    // A local prefix must never become a playlist, URI redirect or adaptive network source.
    if (!kind || strstr(kind, "Source") || strstr(kind, "Network") || strstr(kind, "Adaptive")) return 2;
    if (strstr(kind, "Demuxer")) return !strcmp(name, "qtdemux") || !strcmp(name, "matroskademux") ? 0 : 2;
    return strstr(kind, "Video") && (strstr(kind, "Decoder") || strstr(kind, "Parser")) ? 0 : 2;
}
static void preview_pad(GstElement *decode, GstPad *pad, gpointer output) {
    (void)decode;
    GstCaps *caps = gst_pad_get_current_caps(pad);
    if (!caps) return;
    if (gst_caps_get_size(caps) && !strcmp(gst_structure_get_name(gst_caps_get_structure(caps, 0)), "video/x-raw")) {
        GstPad *sink = gst_element_get_static_pad(GST_ELEMENT(output), "sink");
        if (!gst_pad_is_linked(sink)) gst_pad_link(pad, sink);
        gst_object_unref(sink);
    }
    gst_caps_unref(caps);
}
GdkTexture *monogram_preview_video(const char *path, GCancellable *cancel, guint timeout_ms) {
    if (g_cancellable_is_cancelled(cancel) || !gst_init_check(NULL, NULL, NULL)) return NULL;
    GstElement *pipeline = gst_pipeline_new(NULL);
    GstElement *source = gst_element_factory_make("filesrc", NULL);
    GstElement *decode = gst_element_factory_make("decodebin", NULL);
    GstElement *convert = gst_element_factory_make("videoconvert", NULL);
    GstElement *scale = gst_element_factory_make("videoscale", NULL);
    GstElement *sink = gst_element_factory_make("appsink", NULL);
    gboolean added = FALSE;
    GdkTexture *texture = NULL;
    if (!pipeline || !source || !decode || !convert || !scale || !sink) goto cleanup;
    GstCaps *caps = gst_caps_from_string("video/x-raw,format=BGRA,width=(int)[1,640],height=(int)[1,640],pixel-aspect-ratio=(fraction)1/1");
    gst_app_sink_set_caps(GST_APP_SINK(sink), caps);
    gst_caps_unref(caps);
    g_object_set(source, "location", path, NULL);
    g_object_set(sink, "max-buffers", 1u, "drop", TRUE, "sync", FALSE, NULL);
    g_signal_connect(decode, "autoplug-select", G_CALLBACK(preview_autoplug), NULL);
    g_signal_connect(decode, "pad-added", G_CALLBACK(preview_pad), convert);
    gst_bin_add_many(GST_BIN(pipeline), source, decode, convert, scale, sink, NULL);
    added = TRUE;
    if (!gst_element_link(source, decode) || !gst_element_link_many(convert, scale, sink, NULL)) goto cleanup;
    gint64 deadline = g_get_monotonic_time() + MIN(timeout_ms, 3000u) * 1000LL;
    if (gst_element_set_state(pipeline, GST_STATE_PAUSED) == GST_STATE_CHANGE_FAILURE) goto cleanup;
    GstSample *sample = NULL;
    GstBus *bus = gst_element_get_bus(pipeline);
    while (!g_cancellable_is_cancelled(cancel) && g_get_monotonic_time() < deadline) {
        sample = gst_app_sink_try_pull_preroll(GST_APP_SINK(sink), 50 * GST_MSECOND);
        if (sample) break;
        GstMessage *terminal = gst_bus_pop_filtered(bus, GST_MESSAGE_ERROR | GST_MESSAGE_EOS);
        if (terminal) { gst_message_unref(terminal); break; }
    }
    gst_object_unref(bus);
    if (sample) {
        GstVideoInfo info;
        GstVideoFrame frame;
        GstBuffer *buffer = gst_sample_get_buffer(sample);
        if (!g_cancellable_is_cancelled(cancel) && gst_video_info_from_caps(&info, gst_sample_get_caps(sample))
            && GST_VIDEO_INFO_WIDTH(&info) > 0 && GST_VIDEO_INFO_WIDTH(&info) <= 640
            && GST_VIDEO_INFO_HEIGHT(&info) > 0 && GST_VIDEO_INFO_HEIGHT(&info) <= 640
            && gst_video_frame_map(&frame, &info, buffer, GST_MAP_READ)) {
            guint width = GST_VIDEO_FRAME_WIDTH(&frame), height = GST_VIDEO_FRAME_HEIGHT(&frame);
            gsize stride = width * 4;
            guint8 *pixels = g_malloc_n(stride, height);
            for (guint y = 0; y < height; y++)
                memcpy(pixels + y * stride, (guint8 *)GST_VIDEO_FRAME_PLANE_DATA(&frame, 0)
                    + y * GST_VIDEO_FRAME_PLANE_STRIDE(&frame, 0), stride);
            GBytes *bytes = g_bytes_new_take(pixels, stride * height);
            texture = gdk_memory_texture_new(width, height, GDK_MEMORY_B8G8R8A8_PREMULTIPLIED, bytes, stride);
            g_bytes_unref(bytes);
            gst_video_frame_unmap(&frame);
        }
        gst_sample_unref(sample);
    }
cleanup:
    if (pipeline) gst_element_set_state(pipeline, GST_STATE_NULL);
    if (!added) {
        if (source) gst_object_unref(source);
        if (decode) gst_object_unref(decode);
        if (convert) gst_object_unref(convert);
        if (scale) gst_object_unref(scale);
        if (sink) gst_object_unref(sink);
    }
    if (pipeline) gst_object_unref(pipeline);
    return texture;
}
