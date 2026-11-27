/* Run under a private GTK display; see docs/MEDIA_AUTHORING.md. */
#include "../native/video_player.c"

static void no_video_widget (GtkWidget *widget) {
  g_assert_false (CLAPPER_GTK_IS_VIDEO (widget));
  g_assert_false (GTK_IS_PICTURE (widget));
  for (GtkWidget *child = gtk_widget_get_first_child (widget); child;
      child = gtk_widget_get_next_sibling (child))
    no_video_widget (child);
}

int main (int argc, char **argv) {
  gtk_init ();
  g_assert_true (monogram_playback_init ());
  g_assert_cmpint (argc, >, 1);
  for (int i = 1; i < argc; i++) {
    char *uri = g_filename_to_uri (argv[i], NULL, NULL);
    GtkWidget *controls, *replay;
    GtkMediaStream *stream = monogram_video_new (uri, &controls, &replay);
    g_free (uri);
    g_assert_nonnull (stream);
    no_video_widget (controls);
    GtkWidget *window = gtk_window_new ();
    gtk_window_set_default_size (GTK_WINDOW (window), 640, 360);
    GtkWidget *overlay = gtk_overlay_new ();
    gtk_window_set_child (GTK_WINDOW (window), overlay);
    GtkWidget *picture = gtk_picture_new_for_paintable (GDK_PAINTABLE (stream));
    gtk_overlay_set_child (GTK_OVERLAY (overlay), picture);
    gtk_widget_set_valign (controls, GTK_ALIGN_END);
    gtk_overlay_add_overlay (GTK_OVERLAY (overlay), controls);
    gtk_window_present (GTK_WINDOW (window));
    gint64 deadline = g_get_monotonic_time () + 10 * G_USEC_PER_SEC;
    while (!gtk_media_stream_is_prepared (stream)) {
      while (g_main_context_iteration (NULL, FALSE));
      g_assert_null (gtk_media_stream_get_error (stream));
      g_assert_cmpint (g_get_monotonic_time (), <, deadline);
      g_usleep (1000);
    }
    g_assert_cmpint (gdk_paintable_get_intrinsic_width (GDK_PAINTABLE (stream)), >, 1);
    int minimum, natural;
    gtk_widget_measure (controls, GTK_ORIENTATION_VERTICAL, 640,
        &minimum, &natural, NULL, NULL);
    g_assert_cmpint (natural, >, 0);
    g_assert_cmpint (natural, <, 100);
    g_assert_true (gtk_widget_activate_action (controls, "av.play", NULL));
    g_assert_true (gtk_widget_activate_action (controls, "av.pause", NULL));
    g_assert_true (gtk_widget_activate_action (controls, "av.seek", "d", 0.0));
    g_print ("controls-only: %s, natural height %d\n", argv[i], natural);
    gtk_window_destroy (GTK_WINDOW (window));
    g_object_unref (stream);
  }
  return 0;
}
