#ifndef SENA_MOBILE_H
#define SENA_MOBILE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The C interface of the SoftPAL runtime for app hosts (crates/sena-mobile).
 *
 * A session runs one game: open it, give it a view to draw into, then call
 * sena_mobile_step once per display frame (CADisplayLink) and pass the input
 * on. The engine draws with Metal (wgpu) into the view's CAMetalLayer, fitted
 * with black bars. Sound is pulled by the host: render
 * sena_mobile_audio_render from its audio callback.
 *
 * Every call except the sena_mobile_audio_* ones belongs on the thread that
 * opened the session (the main thread). Strings are UTF-8 and borrowed for the
 * duration of the call. */

#define SENA_MOBILE_ABI_VERSION 1u

typedef struct SenaMobileSession SenaMobileSession;
typedef struct SenaMobileAudio SenaMobileAudio;

typedef struct {
    uint32_t struct_size;  /* sizeof(SenaMobileConfig) */
    const char *game_dir;  /* the game folder (data.pac, *.pac); saves go to its save/ */
    uint32_t encoding;     /* one of SENA_MOBILE_ENCODING_* */
    uint32_t language;     /* one of SENA_MOBILE_LANGUAGE_* */
} SenaMobileConfig;

enum {
    SENA_MOBILE_ENCODING_AUTOMATIC = 0, /* what a translation patch in the folder
                                           implies (see sena_mobile_open), else
                                           Shift-JIS */
    SENA_MOBILE_ENCODING_SHIFT_JIS = 1,
    SENA_MOBILE_ENCODING_GBK = 2,
    SENA_MOBILE_ENCODING_UTF8 = 3,
};

/* Which text of the multi-language releases (the official Chinese FHD ones
 * carry Japanese, Simplified and Traditional Chinese in one file). A choice
 * the player made in the game's own settings wins over it. */
enum {
    SENA_MOBILE_LANGUAGE_AUTOMATIC = 0, /* Simplified Chinese when the folder has
                                           the Chinese archives, else Japanese */
    SENA_MOBILE_LANGUAGE_JAPANESE = 1,
    SENA_MOBILE_LANGUAGE_SIMPLIFIED_CHINESE = 2,
    SENA_MOBILE_LANGUAGE_TRADITIONAL_CHINESE = 3,
};

enum {
    SENA_MOBILE_RUNNING = 0,
    SENA_MOBILE_ENDED = 1,  /* the game quit */
    SENA_MOBILE_FAILED = 2, /* see sena_mobile_last_error: a command the runtime
                               can't run, a broken file... The last frame
                               stays on screen. */
};

enum {
    SENA_MOBILE_POINTER_MOVE = 0,
    SENA_MOBILE_POINTER_LEFT_DOWN = 1,
    SENA_MOBILE_POINTER_LEFT_UP = 2,
    SENA_MOBILE_POINTER_RIGHT_DOWN = 3,
    SENA_MOBILE_POINTER_RIGHT_UP = 4,
    SENA_MOBILE_POINTER_MIDDLE_DOWN = 5,
    SENA_MOBILE_POINTER_MIDDLE_UP = 6,
};

uint32_t sena_mobile_abi_version(void);

/* The last error on this thread, or "" when none. Valid until the next call
 * on this thread. */
const char *sena_mobile_last_error(void);

/* Engine log records: level 1 error, 2 warn, 3 info, 4 debug, 5 trace.
 * Strings are valid during the call; any thread. NULL stops them. Debug and
 * trace are very verbose (every script command). */
typedef void (*sena_mobile_log_callback)(int32_t level, const char *target, const char *message, void *context);
void sena_mobile_set_log_callback(sena_mobile_log_callback callback, void *context, int32_t max_level);

/* The text face for sessions opened after the call: a TTF, OTF or TTC file
 * and the face in it, as a PostScript or family name ("HiraginoSans-W3") or a
 * collection index ("2"); NULL or "" for the first face. It draws the text the game's own bitmap font
 * (DEFAULT_FONT.DAT) does not cover and the text of the system font types.
 * No face is bundled with the runtime; the last face registered is used.
 * Returns 1, or 0 when the file is missing, not a font or has no such face
 * (the previous face stays). */
int32_t sena_mobile_register_font(const char *path, const char *face);

/* Opens and boots a game. NULL on failure (see sena_mobile_last_error). */
SenaMobileSession *sena_mobile_open(const SenaMobileConfig *config);

/* The game's screen size in pixels (SYSTEM.INI: 1280 x 720, 1920 x 1080...). */
void sena_mobile_screen_size(const SenaMobileSession *session, uint32_t *width, uint32_t *height);

/* Draw into ui_view, a UIView whose layer is a CAMetalLayer, of
 * width x height drawable pixels. Call again after the size changed. The
 * view must outlive the session or sena_mobile_detach_surface. Returns 0, or
 * -1 on failure (see sena_mobile_last_error). */
int32_t sena_mobile_set_surface(SenaMobileSession *session, void *ui_view, uint32_t width_px, uint32_t height_px);
void sena_mobile_detach_surface(SenaMobileSession *session);

/* Advances the game by dt_ms (clamped to 1..250) and draws the frame.
 * Returns SENA_MOBILE_RUNNING, _ENDED or _FAILED. */
int32_t sena_mobile_step(SenaMobileSession *session, uint32_t dt_ms);

/* The pointer, in drawable pixels of the surface. One of
 * SENA_MOBILE_POINTER_*. */
void sena_mobile_pointer(SenaMobileSession *session, int32_t phase, float x, float y);
/* Mouse wheel: positive notches up (the backlog), negative down. */
void sena_mobile_wheel(SenaMobileSession *session, int32_t notches);
/* Windows virtual-key code: 0x0D Enter, 0x1B Escape, 0x11 Ctrl (skip),
 * 0x20 Space, 0x25..0x28 arrows, 0x70.. F1.., '0'..'9', 'A'..'Z'. */
void sena_mobile_key(SenaMobileSession *session, int32_t vk, int32_t down);
/* The app left (0) or regained (1) the foreground. */
void sena_mobile_set_focused(SenaMobileSession *session, int32_t focused);

/* The last frame drawn on the CPU at the game's screen size: RGBA, rows top
 * down, width * 4 bytes each. Valid until the next call on the session; NULL
 * before the first frame. */
const uint8_t *sena_mobile_snapshot(SenaMobileSession *session, uint32_t *width, uint32_t *height);

/* Sound: an output fed from the session's mixer by a thread of its own.
 * sena_mobile_audio_render never blocks or allocates, so it can run on a
 * real-time audio thread; call it from one thread at a time. Release it after
 * sena_mobile_close. Button reactions and voice waits finish as their sound
 * plays, so the host must keep rendering while the game runs. */
SenaMobileAudio *sena_mobile_audio(SenaMobileSession *session);
uint32_t sena_mobile_audio_sample_rate(void);
/* Writes frames interleaved stereo float samples (2 * frames floats). */
void sena_mobile_audio_render(SenaMobileAudio *audio, float *out, uint32_t frames);
void sena_mobile_audio_release(SenaMobileAudio *audio);

/* Ends the session. The game saves its settings itself when they change. */
void sena_mobile_close(SenaMobileSession *session);

#ifdef __cplusplus
}
#endif

#endif
