// SPDX-License-Identifier: GPL-3.0-or-later
// Built instead of hdr_image.c when the `hdr` feature is off or libultrahdr is absent.
// Reporting "no HDR" (0) makes the media helper fall back to the ordinary decode path.
int monogram_hdr_image(unsigned *original_width,unsigned *original_height) {
    (void)original_width;
    (void)original_height;
    return 0;
}
