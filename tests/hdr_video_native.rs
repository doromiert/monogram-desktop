use super::*;
use gtk::{gdk,gsk};

fn texture_states(node:&gsk::RenderNode,states:&mut Vec<(gdk::ColorState,gdk::MemoryFormat)>) {
    use gsk::RenderNodeType::*;
    match node.node_type() {
        TextureNode=>{let t=node.clone().downcast::<gsk::TextureNode>().unwrap().texture();states.push((t.color_state(),t.format()));},
        TextureScaleNode=>{let t=node.clone().downcast::<gsk::TextureScaleNode>().unwrap().texture();states.push((t.color_state(),t.format()));},
        ContainerNode=>{let n=node.clone().downcast::<gsk::ContainerNode>().unwrap();for i in 0..n.n_children(){texture_states(&n.child(i),states);}}
        TransformNode=>texture_states(&node.clone().downcast::<gsk::TransformNode>().unwrap().child(),states),
        ClipNode=>texture_states(&node.clone().downcast::<gsk::ClipNode>().unwrap().child(),states),
        RoundedClipNode=>texture_states(&node.clone().downcast::<gsk::RoundedClipNode>().unwrap().child(),states),
        OpacityNode=>texture_states(&node.clone().downcast::<gsk::OpacityNode>().unwrap().child(),states),
        _=>{}
    }
}

#[gtk::test]
#[ignore = "requires a private GTK display and the upstream GTK video sink"]
fn hdr10_and_hlg_retain_color_in_playback_and_posters() {
    adw::init().unwrap();assert!(initialize());
    for (name,transfer) in [("pq",16),("hlg",18)] {
        let path=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/hdr/{name}.mp4"));
        let player=Playback::new(VerifiedFile(std::fs::File::open(&path).unwrap())).expect("color-managed video sink");
        let deadline=Instant::now()+Duration::from_secs(10);let context=glib::MainContext::default();
        loop {
            while context.pending(){context.iteration(false);}
            let snapshot=gtk::Snapshot::new();player.media.snapshot(&snapshot,160.,90.);
            let mut states=vec![];if let Some(node)=snapshot.to_node(){texture_states(&node,&mut states);}
            if let Some((_,format))=states.iter().find(|(s,_)|s.create_cicp_params().is_some_and(|p|p.color_primaries()==9 && p.transfer_function()==transfer)){
                let precision=format!("{format:?}");
                assert!(precision.contains("10") || precision.contains("16") || precision.contains("Float"),"{name} sink reduced precision: {precision}");
                eprintln!("{name} native video format: {precision}");break;
            }
            assert!(player.media.error().is_none(),"{:?}",player.media.error());
            assert!(Instant::now()<deadline,"{name} playback lost its HDR color state");
            std::thread::sleep(Duration::from_millis(10));
        }
        let preview=crate::image_loader::encode_video_preview(&path,&gio::Cancellable::new(),1500).unwrap();
        let mut decoder=crate::image_loader::Decoder::open_bytes(&glib::Bytes::from_owned(preview.bytes),&gio::Cancellable::new()).unwrap();
        let params=decoder.frame(320).unwrap().texture.color_state().create_cicp_params().unwrap();
        assert_eq!((params.color_primaries(),params.transfer_function()),(9,transfer),"{name} poster flattened HDR");
        player.media.pause();drop(player);
    }
}
