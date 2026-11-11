/* Receives a screen-cast stream from PipeWire, which is how GNOME and KDE hand out the screen's picture.
   PipeWire's library is loaded at run time, so the binary starts on machines that do not have it. */
#include <dlfcn.h>
#include <pthread.h>
#include <stdlib.h>
#include <time.h>
#include <pipewire/context.h>
#include <pipewire/core.h>
#include <pipewire/keys.h>
#include <pipewire/main-loop.h>
#include <pipewire/stream.h>
#include <spa/param/video/format-utils.h>

/* The library's functions this file calls. They are reached through the pointers in `pw` only. */
#define FUNCTIONS(F) \
    F(pw_main_loop_new) F(pw_main_loop_get_loop) F(pw_main_loop_destroy) \
    F(pw_context_new) F(pw_context_connect) F(pw_context_destroy) F(pw_core_disconnect) \
    F(pw_properties_new) F(pw_stream_new) F(pw_stream_add_listener) F(pw_stream_connect) \
    F(pw_stream_update_params) F(pw_stream_dequeue_buffer) F(pw_stream_queue_buffer) F(pw_stream_destroy)

static struct {
    void (*pw_init)(int *argc, char ***argv);
#define F(name) __typeof__(name) *name;
    FUNCTIONS(F)
#undef F
} pw;

static pthread_once_t once = PTHREAD_ONCE_INIT;
static int loaded;

static void load(void)
{
    void *library = dlopen("libpipewire-0.3.so.0", RTLD_NOW);
    if (!library || !(pw.pw_init = dlsym(library, "pw_init")))
        return;
#define F(name) \
    if (!(pw.name = dlsym(library, #name))) \
        return;
    FUNCTIONS(F)
#undef F
    pw.pw_init(NULL, NULL);
    loaded = 1;
}

struct screen {
    struct pw_main_loop *loop;
    struct pw_context *context;
    struct pw_core *core;
    struct pw_stream *stream;
    struct spa_hook listener;
    struct spa_video_info_raw format;
    /* The buffer whose pixels the caller is looking at; it goes back to the stream on the next call. */
    struct pw_buffer *held;
    int ended;
};

static void on_state_changed(void *data, enum pw_stream_state old, enum pw_stream_state state, const char *error)
{
    struct screen *screen = data;
    (void)old;
    (void)error;
    /* The stream starts out unconnected and only comes back to that state when its source has gone. */
    if (state == PW_STREAM_STATE_ERROR || state == PW_STREAM_STATE_UNCONNECTED)
        screen->ended = 1;
}

static void on_param_changed(void *data, uint32_t id, const struct spa_pod *param)
{
    struct screen *screen = data;
    if (id != SPA_PARAM_Format || !param)
        return;
    if (spa_format_video_raw_parse(param, &screen->format) < 0) {
        screen->ended = 1;
        return;
    }
    /* The format is settled; say which kinds of buffer are acceptable: plain memory, not GPU buffers. */
    uint8_t storage[256];
    struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(storage, sizeof storage);
    const struct spa_pod *buffers = spa_pod_builder_add_object(&builder,
        SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
        SPA_PARAM_BUFFERS_dataType, SPA_POD_CHOICE_FLAGS_Int((1 << SPA_DATA_MemPtr) | (1 << SPA_DATA_MemFd)));
    pw.pw_stream_update_params(screen->stream, &buffers, 1);
}

static const struct pw_stream_events events = {
    PW_VERSION_STREAM_EVENTS,
    .state_changed = on_state_changed,
    .param_changed = on_param_changed,
};

void screen_close(struct screen *screen)
{
    if (screen->stream)
        pw.pw_stream_destroy(screen->stream);
    if (screen->core)
        pw.pw_core_disconnect(screen->core);
    if (screen->context)
        pw.pw_context_destroy(screen->context);
    if (screen->loop)
        pw.pw_main_loop_destroy(screen->loop);
    free(screen);
}

/* Starts receiving the stream of PipeWire node `node`, asking for at most `fps` pictures a second.
   NULL if PipeWire is not available or refuses. */
struct screen *screen_open(uint32_t node, int fps)
{
    pthread_once(&once, load);
    if (!loaded)
        return NULL;
    struct screen *screen = calloc(1, sizeof *screen);
    if (!screen)
        return NULL;
    if (!(screen->loop = pw.pw_main_loop_new(NULL))
        || !(screen->context = pw.pw_context_new(pw.pw_main_loop_get_loop(screen->loop), NULL, 0))
        || !(screen->core = pw.pw_context_connect(screen->context, NULL, 0))
        || !(screen->stream = pw.pw_stream_new(screen->core, "Remote Desktop",
                 pw.pw_properties_new(PW_KEY_MEDIA_TYPE, "Video", PW_KEY_MEDIA_CATEGORY, "Capture", PW_KEY_MEDIA_ROLE, "Screen", NULL)))) {
        screen_close(screen);
        return NULL;
    }
    pw.pw_stream_add_listener(screen->stream, &screen->listener, &events, screen);

    /* Wanted: pixels as blue, green, red and a fourth byte, at any size, and no faster than the frame rate.
       Not naming a GPU buffer modifier is what asks for plain memory. */
    uint8_t storage[1024];
    struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(storage, sizeof storage);
    const struct spa_pod *format = spa_pod_builder_add_object(&builder,
        SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
        SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
        SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        SPA_FORMAT_VIDEO_format, SPA_POD_CHOICE_ENUM_Id(3, SPA_VIDEO_FORMAT_BGRx, SPA_VIDEO_FORMAT_BGRx, SPA_VIDEO_FORMAT_BGRA),
        SPA_FORMAT_VIDEO_size, SPA_POD_CHOICE_RANGE_Rectangle(&SPA_RECTANGLE(1920, 1080), &SPA_RECTANGLE(1, 1), &SPA_RECTANGLE(16384, 16384)),
        SPA_FORMAT_VIDEO_framerate, SPA_POD_Fraction(&SPA_FRACTION(0, 1)),
        SPA_FORMAT_VIDEO_maxFramerate, SPA_POD_CHOICE_RANGE_Fraction(&SPA_FRACTION(fps, 1), &SPA_FRACTION(1, 1), &SPA_FRACTION(fps, 1)));
    if (pw.pw_stream_connect(screen->stream, PW_DIRECTION_INPUT, node, PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS, &format, 1) < 0) {
        screen_close(screen);
        return NULL;
    }
    return screen;
}

/* Whether a buffer holds a whole picture in the agreed format. One without pixels only reports something else,
   such as a pointer movement; one that claims more than its memory holds is not to be read. */
static int holds_picture(const struct screen *screen, const struct pw_buffer *buffer)
{
    const struct spa_data *data = &buffer->buffer->datas[0];
    if (!data->data || !data->chunk->size || data->chunk->stride < (int)screen->format.size.width * 4)
        return 0;
    return data->chunk->offset + (uint64_t)data->chunk->stride * screen->format.size.height <= data->maxsize;
}

static int64_t now_ms(void)
{
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (int64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

/* Waits up to `timeout_ms` for a new picture. Returns 1 and describes the picture, which stays valid until the
   next call; 0 if none came; -1 once the stream has ended. Of several waiting pictures only the newest is kept. */
int screen_next(struct screen *screen, int timeout_ms, const uint8_t **pixels, int *stride, int *width, int *height)
{
    struct pw_loop *loop = pw.pw_main_loop_get_loop(screen->loop);
    int64_t deadline = now_ms() + timeout_ms;
    if (screen->held) {
        pw.pw_stream_queue_buffer(screen->stream, screen->held);
        screen->held = NULL;
    }
    pw_loop_enter(loop);
    for (;;) {
        struct pw_buffer *buffer;
        while ((buffer = pw.pw_stream_dequeue_buffer(screen->stream))) {
            struct pw_buffer *unused = holds_picture(screen, buffer) ? screen->held : buffer;
            if (unused != buffer)
                screen->held = buffer;
            if (unused)
                pw.pw_stream_queue_buffer(screen->stream, unused);
        }
        int64_t left = deadline - now_ms();
        if (screen->held || screen->ended || left <= 0)
            break;
        pw_loop_iterate(loop, (int)left);
    }
    pw_loop_leave(loop);
    if (screen->ended)
        return -1;
    if (!screen->held)
        return 0;
    struct spa_data *data = &screen->held->buffer->datas[0];
    *pixels = SPA_PTROFF(data->data, data->chunk->offset, const uint8_t);
    *stride = data->chunk->stride;
    *width = screen->format.size.width;
    *height = screen->format.size.height;
    return 1;
}
