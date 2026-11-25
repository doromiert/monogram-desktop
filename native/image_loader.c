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

/* Keep CICP and precision when thumbnails need CPU scaling. Cairo's ARGB32
 * conversion is intentionally reserved for sRGB; floating linear inputs are
 * encoded as PQ before integer interpolation so highlights are not clamped. */
GdkTexture *monogram_texture_fit_color(GdkTexture *texture, guint edge) {
    guint width=gdk_texture_get_width(texture), height=gdk_texture_get_height(texture);
    if (!dimensions(width,height) || !edge || edge>8192) return NULL;
    if (width<=edge && height<=edge) return g_object_ref(texture);
    double ratio=(double)edge/MAX(width,height);
    guint w=MAX(1,(guint)(width*ratio+.5)), h=MAX(1,(guint)(height*ratio+.5));
    GdkColorState *color=gdk_texture_get_color_state(texture);
    GdkMemoryFormat format=gdk_texture_get_format(texture);
    if (format==GDK_MEMORY_R16G16B16_FLOAT || format==GDK_MEMORY_R16G16B16A16_FLOAT
        || format==GDK_MEMORY_R16G16B16A16_FLOAT_PREMULTIPLIED
        || format==GDK_MEMORY_R32G32B32_FLOAT || format==GDK_MEMORY_R32G32B32A32_FLOAT
        || format==GDK_MEMORY_R32G32B32A32_FLOAT_PREMULTIPLIED)
        color=gdk_color_state_get_rec2100_pq();
    guint16 *input=g_try_malloc_n((gsize)width*height,8), *output=g_try_malloc_n((gsize)w*h,8);
    if (!input || !output) {g_free(input);g_free(output);return NULL;}
    GdkTextureDownloader *download=gdk_texture_downloader_new(texture);
    gdk_texture_downloader_set_color_state(download,color);
    gdk_texture_downloader_set_format(download,GDK_MEMORY_R16G16B16A16_PREMULTIPLIED);
    gdk_texture_downloader_download_into(download,(guchar *)input,width*8);
    gdk_texture_downloader_free(download);
    for (guint y=0;y<h;y++) {
        double sy=CLAMP((y+.5)*height/h-.5,0.,height-1.);
        guint y0=(guint)sy,y1=MIN(y0+1,height-1);double fy=sy-y0;
        for (guint x=0;x<w;x++) {
            double sx=CLAMP((x+.5)*width/w-.5,0.,width-1.);
            guint x0=(guint)sx,x1=MIN(x0+1,width-1);double fx=sx-x0;
            for (guint c=0;c<4;c++) {
                double top=input[((gsize)y0*width+x0)*4+c]*(1-fx)+input[((gsize)y0*width+x1)*4+c]*fx;
                double bottom=input[((gsize)y1*width+x0)*4+c]*(1-fx)+input[((gsize)y1*width+x1)*4+c]*fx;
                output[((gsize)y*w+x)*4+c]=(guint16)(top*(1-fy)+bottom*fy+.5);
            }
        }
    }
    g_free(input);
    GBytes *bytes=g_bytes_new_take(output,(gsize)w*h*8);
    GdkMemoryTextureBuilder *builder=gdk_memory_texture_builder_new();
    gdk_memory_texture_builder_set_width(builder,w);gdk_memory_texture_builder_set_height(builder,h);
    gdk_memory_texture_builder_set_format(builder,GDK_MEMORY_R16G16B16A16_PREMULTIPLIED);
    gdk_memory_texture_builder_set_color_state(builder,color);
    gdk_memory_texture_builder_set_stride(builder,w*8);gdk_memory_texture_builder_set_bytes(builder,bytes);
    GdkTexture *result=gdk_memory_texture_builder_build(builder);
    g_object_unref(builder);g_bytes_unref(bytes);return result;
}
