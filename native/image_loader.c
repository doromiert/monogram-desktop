// SPDX-License-Identifier: GPL-3.0-or-later
// Thin binding to the system's upstream sandboxed image loader.
#include <glycin-gtk4.h>
#include <gio/gunixinputstream.h>

static gboolean dimensions(guint32 width, guint32 height) {
    return width && height && width <= 65535 && height <= 65535
        && (guint64)width * height <= 64000000;
}
GInputStream *monogram_verified_stream_new(int fd) {
    return g_unix_input_stream_new(fd, TRUE);
}

GlyImage *monogram_image_open(GInputStream *stream, GCancellable *cancel, GError **error) {
    GlyLoader *loader = gly_loader_new_for_stream(stream);
    g_object_set(loader, "cancellable", cancel, NULL);
    // This Linux package never silently selects an unsandboxed development mode.
    gly_loader_set_sandbox_selector(loader, GLY_SANDBOX_SELECTOR_BWRAP);
    gly_loader_set_accepted_memory_formats(loader, GLY_MEMORY_SELECTION_B8G8R8A8_PREMULTIPLIED);
    GlyImage *image = gly_loader_load(loader, error);
    g_object_unref(loader);
    if (image && !dimensions(gly_image_get_width(image), gly_image_get_height(image))) {
        g_clear_object(&image);
        g_set_error_literal(error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA, "Image dimensions exceed preview limits");
    }
    return image;
}

GdkTexture *monogram_image_frame(GlyImage *image, guint32 edge, gint64 *delay, GError **error) {
    GlyFrameRequest *request = gly_frame_request_new();
    gly_frame_request_set_scale(request, edge, edge);
    gly_frame_request_set_loop_animation(request, TRUE);
    GlyFrame *frame = gly_image_get_specific_frame(image, request, error);
    g_object_unref(request);
    if (!frame) return NULL;
    GdkTexture *texture = NULL;
    if (dimensions(gly_frame_get_width(frame), gly_frame_get_height(frame))) {
        *delay = gly_frame_get_delay(frame);
        texture = gly_gtk_frame_get_texture(frame);
    } else {
        g_set_error_literal(error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA, "Image frame exceeds preview limits");
    }
    g_object_unref(frame);
    return texture;
}
