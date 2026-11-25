// Private display/namespaces fixture: native precision survives decode, resize and warm cache.
#[gtk::test]
#[ignore = "requires private display, glycin/bubblewrap and the current media helper"]
fn hdr_images_keep_color_and_precision_through_thumbnail_cache() {
    adw::init().unwrap();
    let root=std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("hdr-test");
    fs::create_dir_all(&root).unwrap();
    let cancel=gio::Cancellable::new();
    for (primaries,transfer) in [(9,16),(9,18),(12,13)] {
        let params=gdk::CicpParams::new();params.set_color_primaries(primaries);params.set_transfer_function(transfer);
        params.set_matrix_coefficients(0);params.set_range(gdk::CicpRange::Full);
        let color=params.build_color_state().unwrap();
        let values=(0..64*32).flat_map(|n|[49152u16+(n%127) as u16,32768,16384,65535]).flat_map(u16::to_ne_bytes).collect::<Vec<_>>();
        let texture=gdk::MemoryTextureBuilder::new().set_width(64).set_height(32)
            .set_format(gdk::MemoryFormat::R16g16b16a16).set_color_state(&color)
            .set_bytes(Some(&glib::Bytes::from_owned(values))).set_stride(64*8).build();
        let original=texture.save_to_png_bytes();
        let source=root.join(format!("{transfer}.png"));fs::write(&source,&original).unwrap();
        let mut decoder=Decoder::open_bytes(&original,&cancel).unwrap();
        let fitted=decoder.frame(16).unwrap().texture;
        assert_eq!((fitted.width(),fitted.height()),(16,8));
        let actual=fitted.color_state().create_cicp_params().unwrap();
        assert_eq!((actual.color_primaries(),actual.transfer_function()),(primaries,transfer));
        assert_eq!(fitted.format(),gdk::MemoryFormat::R16g16b16a16Premultiplied);
        let path=root.join(format!("{transfer}.preview"));let stamp=SourceStamp::new(&source.metadata().unwrap());
        let image=crate::image_ui::Image{texture:fitted,mime:"image/png".into(),width:64,height:32,animated:false,motion:None};
        save_preview(&path,stamp.clone(),&image,&cancel);
        let cached=cached_preview(&path,&stamp,16,&cancel,false).unwrap();
        let actual=cached.texture.color_state().create_cicp_params().unwrap();
        assert_eq!((actual.color_primaries(),actual.transfer_function()),(primaries,transfer),"warm cache flattened color");
        assert!(!matches!(cached.texture.format(),gdk::MemoryFormat::B8g8r8a8Premultiplied|gdk::MemoryFormat::R8g8b8a8|gdk::MemoryFormat::R8g8b8a8Premultiplied));
    }
    // Floating extended-range data must not pass through the sRGB/Cairo8 path.
    let float_bytes=(0..64*32).flat_map(|_|[8f32,4.,2.,1.]).flat_map(f32::to_ne_bytes).collect::<Vec<_>>();
    let floating=gdk::MemoryTextureBuilder::new().set_width(64).set_height(32)
        .set_format(gdk::MemoryFormat::R32g32b32a32Float).set_color_state(&gdk::ColorState::srgb_linear())
        .set_bytes(Some(&glib::Bytes::from_owned(float_bytes))).set_stride(64*16).build();
    let scaled=fit(floating,16).unwrap();
    assert_eq!(scaled.color_state().create_cicp_params().unwrap().transfer_function(),16);
    let input=crate::downloads::VerifiedFile(fs::File::open(crate::widgets::asset("../tests/fixtures/hdr/gainmap.jpg")).unwrap());
    let check=crate::media_author_run::hdr_image(&input,&cancel);
    assert!(matches!(&check,Ok(Some(_))),"HDR helper failed: {:?}",check.as_ref().map(|value|value.as_ref().map(|(bytes,w,h)|(bytes.len(),w,h))));
    let mut hdr=Decoder::open(input,&cancel).unwrap();assert_eq!(hdr.mime,"image/jpeg");
    assert_eq!((hdr.width,hdr.height),(64,32));
    let texture=hdr.frame(16).unwrap().texture;
    assert_eq!(texture.color_state().create_cicp_params().unwrap().transfer_function(),16,"JPEG gain map was displayed as SDR base");
    assert_eq!((texture.width(),texture.height()),(16,8));
    let input=crate::downloads::VerifiedFile(fs::File::open(crate::widgets::asset("../tests/fixtures/hdr/gainmap-90.jpg")).unwrap());
    let mut portrait=Decoder::open(input,&cancel).unwrap();
    assert_eq!((portrait.width,portrait.height),(32,64),"gain-map EXIF orientation was lost");
    let texture=portrait.frame(16).unwrap().texture;assert_eq!((texture.width(),texture.height()),(8,16));

}
