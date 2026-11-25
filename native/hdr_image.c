// SPDX-License-Identifier: GPL-3.0-or-later
// Called only by the resource-limited, network-free media helper.
#include <gtk/gtk.h>
#include <ultrahdr_api.h>
#include <stdio.h>
#include <string.h>

GdkTexture *monogram_texture_fit_color(GdkTexture *, guint);
static unsigned exif_orientation(const uhdr_mem_block_t *block) {
    if (!block || block->data_sz<14) return 1;
    const unsigned char *p=block->data;size_t n=block->data_sz;
    if (n>=6 && !memcmp(p,"Exif\0\0",6)) {p+=6;n-=6;}
    if (n<8 || (memcmp(p,"II",2) && memcmp(p,"MM",2))) return 1;
    gboolean le=p[0]=='I';
    #define U16(q) (le?((unsigned)(q)[0]|((unsigned)(q)[1]<<8)):((unsigned)(q)[1]|((unsigned)(q)[0]<<8)))
    #define U32(q) (le?((guint32)U16(q)|((guint32)U16((q)+2)<<16)):((guint32)U16((q)+2)|((guint32)U16(q)<<16)))
    if (U16(p+2)!=42) return 1;
    guint32 offset=U32(p+4);
    if (offset>n-2) return 1;
    unsigned count=MIN(U16(p+offset),256u);
    for (unsigned i=0;i<count;i++) {
        size_t pos=(size_t)offset+2+(size_t)i*12;
        if (pos>n || n-pos<12) break;
        const unsigned char *entry=p+pos;
        if (U16(entry)==0x112 && U16(entry+2)==3 && U32(entry+4)==1) {
            unsigned o=U16(entry+8);return o>=1 && o<=8?o:1;
        }
    }
    #undef U16
    #undef U32
    return 1;
}

int monogram_hdr_image(unsigned *original_width,unsigned *original_height) {
    GMappedFile *mapped=g_mapped_file_new("/in/source",FALSE,NULL);
    if (!mapped) return -1;
    gsize length=g_mapped_file_get_length(mapped);
    if (!length || length>128*1024*1024) {g_mapped_file_unref(mapped);return -1;}
    uhdr_codec_private_t *decoder=uhdr_create_decoder();
    if (!decoder) {g_mapped_file_unref(mapped);return -1;}
    uhdr_compressed_image_t input={.data=g_mapped_file_get_contents(mapped),.data_sz=length,.capacity=length,
        .cg=UHDR_CG_UNSPECIFIED,.ct=UHDR_CT_UNSPECIFIED,.range=UHDR_CR_UNSPECIFIED};
    int result=0;
    // Probe closes the configuration phase in libultrahdr 1.4.
    if (uhdr_dec_set_image(decoder,&input).error_code!=UHDR_CODEC_OK
        || uhdr_dec_set_out_img_format(decoder,UHDR_IMG_FMT_32bppRGBA1010102).error_code!=UHDR_CODEC_OK
        || uhdr_dec_set_out_color_transfer(decoder,UHDR_CT_PQ).error_code!=UHDR_CODEC_OK
        || uhdr_dec_probe(decoder).error_code!=UHDR_CODEC_OK) goto done;
    int width=uhdr_dec_get_image_width(decoder),height=uhdr_dec_get_image_height(decoder);
    if (width<=0 || height<=0 || width>65535 || height>65535 || (guint64)width*height>64000000) {result=-1;goto done;}
    if (uhdr_decode(decoder).error_code!=UHDR_CODEC_OK) {result=-1;goto done;}
    uhdr_raw_image_t *raw=uhdr_get_decoded_image(decoder);
    if (!raw || raw->w!=(unsigned)width || raw->h!=(unsigned)height || raw->fmt!=UHDR_IMG_FMT_32bppRGBA1010102
        || raw->stride[0]<(unsigned)width || !raw->planes[0]) {result=-1;goto done;}
    unsigned orientation=exif_orientation(uhdr_dec_get_exif(decoder));
    gboolean transpose=orientation>=5;
    unsigned w=transpose?height:width,h=transpose?width:height;
    // Bound the derived buffer even for a 64MP source; the decoder's original
    // stays in this disposable process, never in the application's image cache.
    double scale=MIN(1.,4096./MAX(w,h));
    unsigned tw=MAX(1,(unsigned)(w*scale+.5)),th=MAX(1,(unsigned)(h*scale+.5));
    guint16 *pixels=g_try_malloc_n((gsize)tw*th,8);
    if (!pixels) {result=-1;goto done;}
    for (unsigned y=0;y<th;y++) for (unsigned x=0;x<tw;x++) {
        double ox=(x+.5)*w/tw-.5,oy=(y+.5)*h/th-.5,rx=ox,ry=oy;
        switch (orientation) {
            case 2:rx=width-1-ox;break;case 3:rx=width-1-ox;ry=height-1-oy;break;
            case 4:ry=height-1-oy;break;case 5:rx=oy;ry=ox;break;
            case 6:rx=oy;ry=height-1-ox;break;case 7:rx=width-1-oy;ry=height-1-ox;break;
            case 8:rx=width-1-oy;ry=ox;break;
        }
        rx=CLAMP(rx,0.,width-1.);ry=CLAMP(ry,0.,height-1.);
        unsigned x0=(unsigned)rx,x1=MIN(x0+1,(unsigned)width-1),y0=(unsigned)ry,y1=MIN(y0+1,(unsigned)height-1);
        double fx=rx-x0,fy=ry-y0;
        guint32 *data=raw->planes[0];
        guint32 a=GUINT32_FROM_LE(data[(gsize)y0*raw->stride[0]+x0]),b=GUINT32_FROM_LE(data[(gsize)y0*raw->stride[0]+x1]);
        guint32 c=GUINT32_FROM_LE(data[(gsize)y1*raw->stride[0]+x0]),d=GUINT32_FROM_LE(data[(gsize)y1*raw->stride[0]+x1]);
        guint16 *out=pixels+((gsize)y*tw+x)*4;
        for (unsigned channel=0;channel<3;channel++) {
            unsigned shift=channel*10;
            double top=((a>>shift)&1023u)*(1-fx)+((b>>shift)&1023u)*fx;
            double bottom=((c>>shift)&1023u)*(1-fx)+((d>>shift)&1023u)*fx;
            out[channel]=(guint16)((top*(1-fy)+bottom*fy)*65535./1023.+.5);
        }
        out[3]=65535;
    }
    GBytes *bytes=g_bytes_new_take(pixels,(gsize)tw*th*8);
    GdkCicpParams *params=gdk_cicp_params_new();
    gdk_cicp_params_set_color_primaries(params,raw->cg==UHDR_CG_BT_2100?9:raw->cg==UHDR_CG_DISPLAY_P3?12:1);
    gdk_cicp_params_set_transfer_function(params,16);gdk_cicp_params_set_matrix_coefficients(params,0);
    gdk_cicp_params_set_range(params,GDK_CICP_RANGE_FULL);
    GdkColorState *color=gdk_cicp_params_build_color_state(params,NULL);g_object_unref(params);
    GdkMemoryTextureBuilder *builder=gdk_memory_texture_builder_new();
    gdk_memory_texture_builder_set_width(builder,tw);gdk_memory_texture_builder_set_height(builder,th);
    gdk_memory_texture_builder_set_format(builder,GDK_MEMORY_R16G16B16A16);
    gdk_memory_texture_builder_set_color_state(builder,color);gdk_color_state_unref(color);
    gdk_memory_texture_builder_set_stride(builder,tw*8);gdk_memory_texture_builder_set_bytes(builder,bytes);
    GdkTexture *texture=gdk_memory_texture_builder_build(builder);g_object_unref(builder);g_bytes_unref(bytes);
    GdkTexture *small=monogram_texture_fit_color(texture,4096);g_object_unref(texture);
    result=small && gdk_texture_save_to_png(small,"/out/result.png")?1:-1;
    if (small) g_object_unref(small);
    *original_width=w;*original_height=h;
done:
    uhdr_release_decoder(decoder);g_mapped_file_unref(mapped);return result;
}
