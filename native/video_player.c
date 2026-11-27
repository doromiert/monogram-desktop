/* Clapper SimpleControls, adapted to the shared GTK paintable.
 * Clapper remains the only decoder; GTK's stream interface preserves inline/arc/zoom rendering. */
#include <clapper-gtk/clapper-gtk.h>
#include <clapper/clapper.h>
#include <gtk/gtk.h>

typedef struct {
  GtkMediaStream parent;
  GtkWidget *controls;
  ClapperPlayer *player;
  GdkPaintable *paintable;
  GtkWidget *replay;
  gboolean has_played;
  gboolean starting;
  gboolean syncing_audio;
  double gtk_volume;
  int width, height;
} MonogramVideo;
typedef GtkMediaStreamClass MonogramVideoClass;
static void paintable_init (GdkPaintableInterface *iface);
G_DEFINE_TYPE_WITH_CODE (MonogramVideo, monogram_video, GTK_TYPE_MEDIA_STREAM,
    G_IMPLEMENT_INTERFACE (GDK_TYPE_PAINTABLE, paintable_init))

static void refresh (MonogramVideo *self) {
  GtkMediaStream *stream = GTK_MEDIA_STREAM (self);
  ClapperQueue *queue = clapper_player_get_queue (self->player);
  ClapperMediaItem *item = clapper_queue_get_current_item (queue);
  double duration = item ? clapper_media_item_get_duration (item) : 0;
  ClapperStreamList *videos = clapper_player_get_video_streams (self->player);
  ClapperStream *video = clapper_stream_list_get_current_stream (videos);
  if (video && CLAPPER_IS_VIDEO_STREAM (video)) {
    int width = clapper_video_stream_get_width (CLAPPER_VIDEO_STREAM (video));
    int height = clapper_video_stream_get_height (CLAPPER_VIDEO_STREAM (video));
    double ratio = self->paintable ? gdk_paintable_get_intrinsic_aspect_ratio (self->paintable) : 0;
    if (ratio > 0 && ((width > height && ratio < 1) || (width < height && ratio > 1))) {
      int swap = width; width = height; height = swap;
    }
    if (self->width != width || self->height != height) {
      self->width = width; self->height = height;
      gdk_paintable_invalidate_size (GDK_PAINTABLE (self));
    }
  }
  g_clear_object (&video);
  /* Clapper can publish the first decoded frame before duration metadata arrives. */
  if (gtk_media_stream_is_prepared (stream) && duration > 0 &&
      gtk_media_stream_get_duration (stream) == 0)
    gtk_media_stream_stream_unprepared (stream);
  gboolean audio = clapper_stream_list_get_n_streams (clapper_player_get_audio_streams (self->player)) > 0;
  gboolean video_ready = self->width > 0 && self->paintable && gdk_paintable_get_intrinsic_width (self->paintable) > 1;
  gboolean audio_ready = audio && clapper_stream_list_get_n_streams (videos) == 0 && duration > 0;
  if (!gtk_media_stream_is_prepared (stream) && (video_ready || audio_ready)) {
    gtk_media_stream_stream_prepared (stream, audio, video_ready, duration > 0, duration * G_USEC_PER_SEC);
  }
  ClapperPlayerState state = clapper_player_get_state (self->player);
  if (state == CLAPPER_PLAYER_STATE_PLAYING || state == CLAPPER_PLAYER_STATE_PAUSED)
    self->has_played = TRUE;
  gboolean ended = state == CLAPPER_PLAYER_STATE_STOPPED ||
      (state == CLAPPER_PLAYER_STATE_PAUSED && duration > 0 &&
       clapper_player_get_position (self->player) >= duration - .05);
  /* Expose Clapper's EOS to still-first motion photos and GTK replay callers. */
  if (ended && self->has_played && !self->starting &&
      !gtk_media_stream_get_loop (stream) && gtk_media_stream_is_prepared (stream) &&
      !gtk_media_stream_get_ended (stream))
    gtk_media_stream_stream_ended (stream);
  if (self->replay) gtk_widget_set_visible (self->replay,
      ended && self->has_played && !gtk_media_stream_get_loop (stream));
  if (item) g_object_unref (item);
  if (gtk_media_stream_is_prepared (stream))
    gtk_media_stream_update (stream, clapper_player_get_position (self->player) * G_USEC_PER_SEC);
}
static void duration_changed (ClapperMediaItem *item, GParamSpec *pspec, MonogramVideo *self) {
  refresh (self);
}
static void state_changed (ClapperPlayer *player, GParamSpec *pspec, MonogramVideo *self) {
  refresh (self);
  /* Native controls and keyboard callers both update the same GTK state. */
  if (clapper_player_get_state (player) == CLAPPER_PLAYER_STATE_BUFFERING) return;
  gboolean playing = clapper_player_get_state (player) == CLAPPER_PLAYER_STATE_PLAYING;
  if (playing) self->starting = FALSE;
  if (!playing && self->starting) return;
  if (gtk_media_stream_get_playing (GTK_MEDIA_STREAM (self)) != playing)
    gtk_media_stream_set_playing (GTK_MEDIA_STREAM (self), playing);
}
static void position_changed (ClapperPlayer *player, GParamSpec *pspec, MonogramVideo *self) {
  refresh (self);
}
static void failed (ClapperPlayer *player, GError *error, gchar *debug, MonogramVideo *self) {
  gtk_media_stream_gerror (GTK_MEDIA_STREAM (self), g_error_copy (error));
}
static void seek_done (ClapperPlayer *player, MonogramVideo *self) {
  if (gtk_media_stream_is_seeking (GTK_MEDIA_STREAM (self)))
    gtk_media_stream_seek_success (GTK_MEDIA_STREAM (self));
}
static void contents_changed (GdkPaintable *paintable, MonogramVideo *self) {
  refresh (self);
  gdk_paintable_invalidate_contents (GDK_PAINTABLE (self));
}
static void size_changed (GdkPaintable *paintable, MonogramVideo *self) {
  refresh (self);
  gdk_paintable_invalidate_size (GDK_PAINTABLE (self));
}
static void loop_changed (MonogramVideo *self) {
  clapper_queue_set_progression_mode (clapper_player_get_queue (self->player),
      gtk_media_stream_get_loop (GTK_MEDIA_STREAM (self))
        ? CLAPPER_QUEUE_PROGRESSION_REPEAT_ITEM : CLAPPER_QUEUE_PROGRESSION_NONE);
}
static gboolean play (GtkMediaStream *stream) {
  MonogramVideo *self = (MonogramVideo *) stream;
  ClapperQueue *queue = clapper_player_get_queue (self->player);
  ClapperMediaItem *item = clapper_queue_get_current_item (queue);
  double duration = item ? clapper_media_item_get_duration (item) : 0;
  if (duration > 0 && clapper_player_get_position (self->player) >= duration - .05)
    clapper_player_seek (self->player, 0);
  if (item) g_object_unref (item);
  self->starting = TRUE;
  clapper_player_play (self->player);
  return TRUE;
}
static void pause_video (GtkMediaStream *stream) {
  MonogramVideo *self = (MonogramVideo *) stream;
  self->starting = FALSE;
  if (self->player) clapper_player_pause (self->player);
}
static void seek (GtkMediaStream *stream, gint64 timestamp) {
  clapper_player_seek (((MonogramVideo *) stream)->player, timestamp / (double) G_USEC_PER_SEC);
}
static void update_audio (GtkMediaStream *stream, gboolean muted, double volume) {
  MonogramVideo *self = (MonogramVideo *) stream;
  if (self->syncing_audio || !self->player) return;
  clapper_player_set_mute (self->player, muted);
  if (volume != self->gtk_volume) clapper_player_set_volume (self->player, volume);
  self->gtk_volume = volume;
}
static void audio_changed (ClapperPlayer *player, GParamSpec *pspec, MonogramVideo *self) {
  self->syncing_audio = TRUE;
  self->gtk_volume = CLAMP (clapper_player_get_volume (player), 0, 1);
  gtk_media_stream_set_muted (GTK_MEDIA_STREAM (self), clapper_player_get_mute (player));
  gtk_media_stream_set_volume (GTK_MEDIA_STREAM (self), self->gtk_volume);
  self->syncing_audio = FALSE;
}
static void snapshot (GdkPaintable *paintable, GdkSnapshot *snapshot, double width, double height) {
  MonogramVideo *self = (MonogramVideo *) paintable;
  if (!self->paintable) return;
  double ratio = self->height > 0 ? self->width / (double) self->height
      : gdk_paintable_get_intrinsic_aspect_ratio (self->paintable);
  if (ratio <= 0) return;
  double video_width = width, video_height = height;
  if (width / height > ratio)
    video_width = height * ratio;
  else
    video_height = width / ratio;
  graphene_point_t offset = GRAPHENE_POINT_INIT (
      (width - video_width) / 2, (height - video_height) / 2);
  gtk_snapshot_save (snapshot);
  gtk_snapshot_translate (snapshot, &offset);
  gtk_snapshot_push_clip (snapshot, &GRAPHENE_RECT_INIT (0, 0, video_width, video_height));
  gdk_paintable_snapshot (self->paintable, snapshot, video_width, video_height);
  gtk_snapshot_pop (snapshot);
  gtk_snapshot_restore (snapshot);
}
static GdkPaintable *current_image (GdkPaintable *paintable) {
  MonogramVideo *self = (MonogramVideo *) paintable;
  return self->paintable ? gdk_paintable_get_current_image (self->paintable)
      : gdk_paintable_new_empty (0, 0);
}
static int intrinsic_width (GdkPaintable *paintable) {
  MonogramVideo *self = (MonogramVideo *) paintable;
  return self->width;
}
static int intrinsic_height (GdkPaintable *paintable) {
  MonogramVideo *self = (MonogramVideo *) paintable;
  return self->height;
}
static void paintable_init (GdkPaintableInterface *iface) {
  iface->snapshot = snapshot;
  iface->get_current_image = current_image;
  iface->get_intrinsic_width = intrinsic_width;
  iface->get_intrinsic_height = intrinsic_height;
}
static void dispose (GObject *object) {
  MonogramVideo *self = (MonogramVideo *) object;
  if (self->player)
    g_signal_handlers_disconnect_by_data (self->player, self);
  if (self->paintable) g_signal_handlers_disconnect_by_data (self->paintable, self);
  g_clear_object (&self->replay);
  /* GtkMediaStream may pause here; STOP must be the final player command. */
  G_OBJECT_CLASS (monogram_video_parent_class)->dispose (object);
  if (self->player) clapper_player_stop (self->player);
  g_clear_object (&self->paintable);
  g_clear_object (&self->controls);
  g_clear_object (&self->player);
}
static void monogram_video_class_init (MonogramVideoClass *klass) {
  G_OBJECT_CLASS (klass)->dispose = dispose;
  klass->play = play;
  klass->pause = pause_video;
  klass->seek = seek;
  klass->update_audio = update_audio;
}
static void monogram_video_init (MonogramVideo *self) { self->gtk_volume = 1; }

static void replay_clicked (GtkButton *button, MonogramVideo *self) {
  gtk_widget_set_visible (self->replay, FALSE);
  clapper_player_seek (self->player, 0);
  clapper_player_play (self->player);
}

/* Clapper only ever plays local, already verified files (/proc/self/fd/N). GStreamer will still
 * follow network URLs that it finds *inside* a file: an HLS, DASH or Smooth Streaming manifest
 * saved as ".mp4" makes it fetch arbitrary hosts, with no DNS pinning or private-address check.
 * Give every network source and adaptive demuxer rank NONE so decodebin and uridecodebin never
 * pick one. Fails closed: if this cannot be established, playback stays unavailable. */
static const char *const blocked_elements[] = {
  "hlsdemux", "hlsdemux2", "dashdemux", "dashdemux2", "mssdemux", "mssdemux2",
  "souphttpsrc", "curlhttpsrc", "rtspsrc", "udpsrc", "tcpclientsrc", "srtsrc", "rtmpsrc", "rtmp2src", NULL
};
static gboolean network_factory (GstElementFactory *factory) {
  const gchar *klass = gst_element_factory_get_metadata (factory, GST_ELEMENT_METADATA_KLASS);
  const gchar *name = gst_plugin_feature_get_name (GST_PLUGIN_FEATURE (factory));
  if (klass && (strstr (klass, "Network") || strstr (klass, "Adaptive"))) return TRUE;
  for (const char *const *blocked = blocked_elements; *blocked; blocked++)
    if (!strcmp (name, *blocked)) return TRUE;
  return FALSE;
}
static void block_network_elements (void) {
  GList *all = gst_registry_get_feature_list (gst_registry_get (), GST_TYPE_ELEMENT_FACTORY);
  for (GList *l = all; l; l = l->next)
    if (network_factory (GST_ELEMENT_FACTORY (l->data)))
      gst_plugin_feature_set_rank (GST_PLUGIN_FEATURE (l->data), GST_RANK_NONE);
  gst_plugin_feature_list_free (all);
}
/* TRUE when no network source or adaptive demuxer is selectable any more. Sources keep working
 * for an explicit http URI (gst_element_make_from_uri ignores rank), but nothing here supplies
 * one: only adaptive demuxers could turn file contents into a network request. */
gboolean monogram_playback_network_blocked (void) {
  GList *all = gst_registry_get_feature_list (gst_registry_get (), GST_TYPE_ELEMENT_FACTORY);
  gboolean blocked = TRUE;
  for (GList *l = all; l; l = l->next)
    if (network_factory (GST_ELEMENT_FACTORY (l->data))
        && gst_plugin_feature_get_rank (GST_PLUGIN_FEATURE (l->data)) != GST_RANK_NONE) blocked = FALSE;
  gst_plugin_feature_list_free (all);
  return blocked;
}
/* Plays `uri` headless for up to `ms` and returns once it errors, ends or the time is up. Used by
 * tests to prove that a manifest saved as a media file does not start a network request. */
void monogram_playback_probe (const char *uri, guint ms) {
  GstElement *play = gst_element_factory_make ("playbin3", NULL);
  GstElement *video = gst_element_factory_make ("fakesink", NULL);
  GstElement *audio = gst_element_factory_make ("fakesink", NULL);
  if (!play || !video || !audio) { if (play) gst_object_unref (play); return; }
  g_object_set (play, "uri", uri, "video-sink", video, "audio-sink", audio, NULL);
  GstBus *bus = gst_element_get_bus (play);
  gst_element_set_state (play, GST_STATE_PLAYING);
  GstMessage *done = gst_bus_timed_pop_filtered (bus, (GstClockTime) ms * GST_MSECOND,
                                                 GST_MESSAGE_ERROR | GST_MESSAGE_EOS);
  if (done) gst_message_unref (done);
  gst_element_set_state (play, GST_STATE_NULL);
  gst_object_unref (bus);
  gst_object_unref (play);
}
gboolean monogram_playback_init (void) {
  if (!clapper_init_check (NULL, NULL)) return FALSE;
  block_network_elements ();
  return monogram_playback_network_blocked ();
}

GtkMediaStream *monogram_video_new (const char *uri, GtkWidget **controls, GtkWidget **replay) {
  MonogramVideo *self = g_object_new (monogram_video_get_type (), NULL);
  /* The picture renders the stream; this owner must never render another video. */
  self->controls = g_object_ref_sink (clapper_gtk_audio_new ());
  self->player = g_object_ref (clapper_gtk_av_get_player (CLAPPER_GTK_AV (self->controls)));
  clapper_player_set_autoplay (self->player, FALSE);
  /* Audio is the public controls-only AV container; restore its video streams. */
  clapper_player_set_video_enabled (self->player, TRUE);
  clapper_player_set_subtitles_enabled (self->player, TRUE);
  GtkWidget *simple = clapper_gtk_simple_controls_new ();
  gtk_widget_set_valign (simple, GTK_ALIGN_END);
  clapper_gtk_simple_controls_set_fullscreenable (CLAPPER_GTK_SIMPLE_CONTROLS (simple), FALSE);
  clapper_gtk_audio_set_child (CLAPPER_GTK_AUDIO (self->controls), simple);
  gtk_widget_add_css_class (self->controls, "galeria-video");
  self->replay = g_object_ref_sink (gtk_button_new_with_label ("Replay"));
  gtk_widget_set_halign (self->replay, GTK_ALIGN_CENTER);
  gtk_widget_set_valign (self->replay, GTK_ALIGN_CENTER);
  gtk_widget_set_visible (self->replay, FALSE);
  gtk_widget_add_css_class (self->replay, "osd");
  gtk_widget_add_css_class (self->replay, "pill");
  g_signal_connect_object (self->replay, "clicked", G_CALLBACK (replay_clicked), self, 0);
  /* Clapper 0.10's own sink tags HDR pixels as sRGB. This upstream GTK
   * sink retains negotiated high-bit-depth formats and CICP color states. */
  GstElement *sink=gst_element_factory_make("gtk4paintablesink",NULL);
  if (sink) {
    gst_object_ref_sink(sink);
    /* Its GTK paintable must be created on this UI thread, before preroll. */
    g_object_get(sink,"paintable",&self->paintable,NULL);
    clapper_player_set_video_sink(self->player,sink);
    gst_object_unref(sink);
  }
  if (!self->paintable) {
    g_object_unref (self);
    return NULL;
  }
  g_signal_connect_object (self->paintable, "invalidate-contents", G_CALLBACK (contents_changed), self, 0);
  g_signal_connect_object (self->paintable, "invalidate-size", G_CALLBACK (size_changed), self, 0);
  g_signal_connect_object (self->player, "notify::state", G_CALLBACK (state_changed), self, 0);
  g_signal_connect_object (self->player, "notify::position", G_CALLBACK (position_changed), self, 0);
  g_signal_connect_object (self->player, "error", G_CALLBACK (failed), self, 0);
  g_signal_connect_object (self->player, "notify::mute", G_CALLBACK (audio_changed), self, 0);
  g_signal_connect_object (self->player, "notify::volume", G_CALLBACK (audio_changed), self, 0);
  g_signal_connect_object (self->player, "seek-done", G_CALLBACK (seek_done), self, 0);
  g_signal_connect (self, "notify::loop", G_CALLBACK (loop_changed), NULL);
  ClapperMediaItem *item = clapper_media_item_new (uri);
  g_signal_connect_object (item, "notify::duration", G_CALLBACK (duration_changed), self, 0);
  clapper_queue_add_item (clapper_player_get_queue (self->player), item);
  g_object_unref (item);
  /* Preroll off-thread in Clapper's own player thread; UI feedback stays immediate. */
  clapper_player_pause (self->player);
  *controls = self->controls;
  *replay = self->replay;
  return GTK_MEDIA_STREAM (self);
}
