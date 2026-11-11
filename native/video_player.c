/* Galeria's ClapperGtk.Video + SimpleControls, adapted to the shared GTK paintable.
 * Clapper remains the only decoder; GTK's stream interface preserves inline/arc/zoom rendering. */
#include <clapper-gtk/clapper-gtk.h>
#include <clapper/clapper.h>
#include <gtk/gtk.h>

typedef struct {
  GtkMediaStream parent;
  GtkWidget *video;
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
  if (!gtk_media_stream_is_prepared (stream) && self->width > 0 && self->paintable &&
      gdk_paintable_get_intrinsic_width (self->paintable) > 1) {
    gboolean audio = clapper_stream_list_get_n_streams (clapper_player_get_audio_streams (self->player)) > 0;
    gtk_media_stream_stream_prepared (stream, audio, TRUE, TRUE, duration * G_USEC_PER_SEC);
  }
  ClapperPlayerState state = clapper_player_get_state (self->player);
  if (state == CLAPPER_PLAYER_STATE_PLAYING || state == CLAPPER_PLAYER_STATE_PAUSED)
    self->has_played = TRUE;
  gboolean ended = state == CLAPPER_PLAYER_STATE_STOPPED ||
      (state == CLAPPER_PLAYER_STATE_PAUSED && duration > 0 &&
       clapper_player_get_position (self->player) >= duration - .05);
  if (self->replay) gtk_widget_set_visible (self->replay,
      ended && self->has_played && !gtk_media_stream_get_loop (stream));
  if (item) g_object_unref (item);
  if (gtk_media_stream_is_prepared (stream))
    gtk_media_stream_update (stream, clapper_player_get_position (self->player) * G_USEC_PER_SEC);
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
  double ratio = self->height > 0 ? self->width / (double) self->height : 0;
  double video_width = width, video_height = height;
  if (ratio > 0 && width / height > ratio)
    video_width = height * ratio;
  else if (ratio > 0)
    video_height = width / ratio;
  graphene_point_t offset = GRAPHENE_POINT_INIT (
      (width - video_width) / 2, (height - video_height) / 2);
  gtk_snapshot_save (snapshot);
  gtk_snapshot_translate (snapshot, &offset);
  gdk_paintable_snapshot (self->paintable, snapshot, video_width, video_height);
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
  g_clear_object (&self->video);
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

static GtkWidget *sink_widget (GstElement *sink) {
  GtkWidget *widget = NULL;
  if (g_object_class_find_property (G_OBJECT_GET_CLASS (sink), "widget"))
    g_object_get (sink, "widget", &widget, NULL);
  if (!widget && GST_IS_BIN (sink)) {
    GstIterator *iterator = gst_bin_iterate_recurse (GST_BIN (sink));
    GValue value = G_VALUE_INIT;
    while (gst_iterator_next (iterator, &value) == GST_ITERATOR_OK) {
      GstElement *child = g_value_get_object (&value);
      if (g_object_class_find_property (G_OBJECT_GET_CLASS (child), "widget"))
        g_object_get (child, "widget", &widget, NULL);
      g_value_unset (&value);
      if (widget) break;
    }
    gst_iterator_free (iterator);
  }
  return widget;
}

static void replay_clicked (GtkButton *button, MonogramVideo *self) {
  gtk_widget_set_visible (self->replay, FALSE);
  clapper_player_seek (self->player, 0);
  clapper_player_play (self->player);
}

gboolean monogram_playback_init (void) { return clapper_init_check (NULL, NULL); }

GtkMediaStream *monogram_video_new (const char *uri, GtkWidget **controls, GtkWidget **replay) {
  MonogramVideo *self = g_object_new (monogram_video_get_type (), NULL);
  self->video = g_object_ref_sink (clapper_gtk_video_new ());
  self->player = g_object_ref (clapper_gtk_av_get_player (CLAPPER_GTK_AV (self->video)));
  clapper_player_set_autoplay (self->player, FALSE);
  clapper_gtk_video_set_fade_delay (CLAPPER_GTK_VIDEO (self->video), 3000);
  GtkWidget *simple = clapper_gtk_simple_controls_new ();
  gtk_widget_set_valign (simple, GTK_ALIGN_END);
  clapper_gtk_video_add_overlay (CLAPPER_GTK_VIDEO (self->video), simple);
  gtk_widget_add_css_class (self->video, "galeria-video");
  self->replay = g_object_ref_sink (gtk_button_new_with_label ("Replay"));
  gtk_widget_set_halign (self->replay, GTK_ALIGN_CENTER);
  gtk_widget_set_valign (self->replay, GTK_ALIGN_CENTER);
  gtk_widget_set_visible (self->replay, FALSE);
  gtk_widget_add_css_class (self->replay, "osd");
  gtk_widget_add_css_class (self->replay, "pill");
  g_signal_connect_object (self->replay, "clicked", G_CALLBACK (replay_clicked), self, 0);
  /* Obtain only the sink picture, excluding Clapper's status/icon paintables. */
  GstElement *sink = clapper_player_get_video_sink (self->player);
  GtkWidget *picture = sink ? sink_widget (sink) : NULL;
  if (picture && GTK_IS_PICTURE (picture)) {
    GdkPaintable *paintable = gtk_picture_get_paintable (GTK_PICTURE (picture));
    if (paintable) self->paintable = g_object_ref (paintable);
    gtk_picture_set_can_shrink (GTK_PICTURE (picture), TRUE);
    gtk_widget_set_opacity (picture, 0);
  }
  g_clear_object (&picture);
  g_clear_object (&sink);
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
  clapper_queue_add_item (clapper_player_get_queue (self->player), item);
  g_object_unref (item);
  /* Preroll off-thread in Clapper's own player thread; UI feedback stays immediate. */
  clapper_player_pause (self->player);
  *controls = self->video;
  *replay = self->replay;
  return GTK_MEDIA_STREAM (self);
}
