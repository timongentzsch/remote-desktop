/* Hardware encoding on NVIDIA GPUs: H.264, at 4:2:0 or with full colour detail, or HEVC at 4:2:0 for a client
   that decodes it. Not HEVC with full colour detail: Chrome on a Mac (158, macOS 27) took it and showed nothing,
   without a word of error. The driver's libraries are loaded at run time, so the binary needs them neither to build nor
   to start; where they are missing or the GPU has no encoder, nvenc_open fails and the caller uses x264. */
/* The loader reports what it cannot find; here a missing library is an ordinary case and needs no message. */
#define FFNV_LOG_FUNC(logctx, msg, ...) ((void)(logctx))
#define FFNV_DEBUG_LOG_FUNC(logctx, msg, ...) ((void)(logctx))
#include <ffnvcodec/dynlink_loader.h>
#include <pthread.h>
#include <string.h>
#include <time.h>
#include "encoder.h"
#include "nal.h"

/* P1 is the fastest preset and P7 the best; P3 costs about a millisecond per frame more than P1. */
#define PRESET NV_ENC_PRESET_P3_GUID
#define TUNING NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY

struct nvenc {
    struct encoder base;
    void *session;
    NV_ENC_INITIALIZE_PARAMS setup;
    NV_ENC_CONFIG config;
    NV_ENC_INPUT_PTR input;
    NV_ENC_OUTPUT_PTR output;
    int width, height, full_chroma;
    /* The id of the last keyframe, before which nothing is kept, and of the last frame, -1 before the first;
       and how many of the last frames are kept. */
    int64_t keyframe, last;
    int kept;
    /* The picture as the caller fills it; it is copied into the driver's buffer for each frame. */
    uint8_t *planes[3];
    /* The last access unit, converted to length-prefixed NAL units. */
    struct nal_units unit;
    uint8_t headers[1024];
};

static const struct encoder_ops nvenc_ops;
static pthread_once_t once = PTHREAD_ONCE_INIT;
static CudaFunctions *cuda;
static NV_ENCODE_API_FUNCTION_LIST api;
/* The GPU context all encoders share. It is made once and kept: making one per encoder costs a tenth of a
   second each time and leaves memory behind in the driver's library. */
static CUcontext context;
static int loaded;

/* Every call into the driver's encoder happens between enter() and leave(), which make the shared context
   current on the calling thread for that long. */
static void enter(void)
{
    cuda->cuCtxPushCurrent(context);
}

static void leave(void)
{
    CUcontext unused;
    cuda->cuCtxPopCurrent(&unused);
}

static void load(void)
{
    NvencFunctions *library = NULL; /* the loader frees whatever a non-null pointer holds */
    uint32_t newest;
    if (cuda_load_functions(&cuda, NULL) < 0 || nvenc_load_functions(&library, NULL) < 0)
        return;
    /* A driver older than the API these headers describe cannot be used. */
    if (library->NvEncodeAPIGetMaxSupportedVersion(&newest) != NV_ENC_SUCCESS || newest < (NVENCAPI_MAJOR_VERSION << 4 | NVENCAPI_MINOR_VERSION))
        return;
    api.version = NV_ENCODE_API_FUNCTION_LIST_VER;
    CUdevice device;
    /* Blocking rather than spinning while the GPU works keeps the processor free. */
    if (library->NvEncodeAPICreateInstance(&api) != NV_ENC_SUCCESS
        || cuda->cuInit(0) != CUDA_SUCCESS
        || cuda->cuDeviceGet(&device, 0) != CUDA_SUCCESS
        || cuda->cuCtxCreate(&context, CU_CTX_SCHED_BLOCKING_SYNC, device) != CUDA_SUCCESS)
        return;
    leave(); /* creating the context made it current on this thread */
    loaded = 1;
}

static void set_rate(NV_ENC_RC_PARAMS *rate, int kbps, int buffer_kbits, int quality)
{
    rate->targetQuality = (uint8_t)quality;
    rate->averageBitRate = rate->maxBitRate = (uint32_t)kbps * 1000;
    rate->vbvBufferSize = rate->vbvInitialDelay = (uint32_t)buffer_kbits * 1000;
}

/* How many of the last frames HEVC keeps for a client that lost those after one of them: as many as a decoder
   holds at level 5.1 for a picture of this size (H.265, A.4.2), less the one it decodes. */
static int hevc_kept_frames(int width, int height)
{
    const long most = 8912896; /* luma samples of the largest picture at level 5.1 */
    long samples = (long)width * height;
    return (samples <= most / 4 ? 16 : samples <= most / 2 ? 12 : samples <= most * 3 / 4 ? 8 : 6) - 1;
}

/* How the colours are meant, as encoder.h says, which H.264 and HEVC state alike. */
static void describe_colours(NV_ENC_CONFIG_H264_VUI_PARAMETERS *vui)
{
    vui->videoSignalTypePresentFlag = 1;
    vui->videoFormat = 5; /* unspecified */
    vui->videoFullRangeFlag = 1;
    vui->colourDescriptionPresentFlag = 1;
    vui->colourPrimaries = vui->transferCharacteristics = vui->colourMatrix = BT709;
    vui->chromaSampleLocationFlag = 1;
    vui->chromaSampleLocationTop = vui->chromaSampleLocationBot = CHROMA_AT_CENTRE;
}

/* One key frame at the start, then every frame refers to the one before it, in one slice, with the parameter
   sets in the stream's configuration record instead of the stream. The last frames are kept for a client that
   lost those after one of them; only the last is searched. */
static void configure_h264(NV_ENC_CONFIG *config, int width, int height, int full_chroma)
{
    config->profileGUID = full_chroma ? NV_ENC_H264_PROFILE_HIGH_444_GUID : NV_ENC_H264_PROFILE_HIGH_GUID;
    NV_ENC_CONFIG_H264 *h264 = &config->encodeCodecConfig.h264Config;
    h264->idrPeriod = NVENC_INFINITE_GOPLENGTH;
    h264->disableSPSPPS = 1;
    h264->chromaFormatIDC = full_chroma ? 3 : 1;
    h264->sliceMode = 3; /* a fixed number of slices per frame: one */
    h264->sliceModeData = 1;
    h264->maxNumRefFrames = h264_kept_frames(width, height);
    h264->numRefL0 = NV_ENC_NUM_REF_FRAMES_1;
    describe_colours(&h264->h264VUIParameters);
}

static void configure_hevc(NV_ENC_CONFIG *config, int width, int height)
{
    config->profileGUID = NV_ENC_HEVC_PROFILE_MAIN_GUID;
    NV_ENC_CONFIG_HEVC *hevc = &config->encodeCodecConfig.hevcConfig;
    hevc->level = NV_ENC_LEVEL_HEVC_51;
    hevc->tier = NV_ENC_TIER_HEVC_MAIN;
    hevc->idrPeriod = NVENC_INFINITE_GOPLENGTH;
    hevc->disableSPSPPS = 1;
    hevc->chromaFormatIDC = 1;
    hevc->sliceMode = 3;
    hevc->sliceModeData = 1;
    hevc->maxNumRefFramesInDPB = hevc_kept_frames(width, height);
    hevc->numRefL0 = NV_ENC_NUM_REF_FRAMES_1;
    describe_colours(&hevc->hevcVUIParameters);
}

static int width_of(const struct nvenc *encoder, int plane)
{
    return plane == 0 || encoder->full_chroma ? encoder->width : encoder->width / 2;
}

static int rows_of(const struct nvenc *encoder, int plane)
{
    return plane == 0 || encoder->full_chroma ? encoder->height : encoder->height / 2;
}

/* Takes an encoder apart and frees it. */
static void destroy(struct nvenc *encoder)
{
    if (encoder->session) {
        enter();
        if (encoder->last >= 0) {
            /* The driver must be told that the stream has ended before its encoder is taken apart. */
            NV_ENC_PIC_PARAMS end = { .version = NV_ENC_PIC_PARAMS_VER, .encodePicFlags = NV_ENC_PIC_FLAG_EOS };
            api.nvEncEncodePicture(encoder->session, &end);
        }
        if (encoder->output)
            api.nvEncDestroyBitstreamBuffer(encoder->session, encoder->output);
        if (encoder->input)
            api.nvEncDestroyInputBuffer(encoder->session, encoder->input);
        api.nvEncDestroyEncoder(encoder->session);
        leave();
    }
    for (int plane = 0; plane < 3; plane++)
        free(encoder->planes[plane]);
    nal_units_free(&encoder->unit);
    free(encoder);
}

/* The driver (seen with version 580) crashes, a moment later and on a thread of its own, when one of several
   encoders is destroyed while another is still in use. So this process has one encoder at a time. A second
   one is asked for when a client reconnects before its old session has wound down, and then the wait is
   short. If the encoder stays taken, because someone else is watching, the newcomer gets none and the caller
   encodes with x264. */
static pthread_mutex_t turn = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t released = PTHREAD_COND_INITIALIZER;
static int taken;
#define TURN_WAIT_SECONDS 1

static int take_turn(void)
{
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += TURN_WAIT_SECONDS;
    pthread_mutex_lock(&turn);
    while (taken && pthread_cond_timedwait(&released, &turn, &deadline) == 0)
        ;
    int mine = !taken;
    if (mine)
        taken = 1;
    pthread_mutex_unlock(&turn);
    return mine;
}

static void give_turn(void)
{
    pthread_mutex_lock(&turn);
    taken = 0;
    pthread_cond_signal(&released);
    pthread_mutex_unlock(&turn);
}

static void nvenc_close(struct encoder *base)
{
    destroy((struct nvenc *)base);
    give_turn();
}

struct encoder *nvenc_open(const struct encoder_setup *setup)
{
    int width = setup->width, height = setup->height, fps = setup->fps, kbps = setup->kbps;
    int full_chroma = setup->full_chroma;
    pthread_once(&once, load);
    if (!loaded || !take_turn())
        return NULL;
    struct nvenc *encoder = calloc(1, sizeof *encoder);
    if (!encoder) {
        give_turn();
        return NULL;
    }
    encoder->last = -1;
    encoder->base.ops = &nvenc_ops;
    encoder->width = width;
    encoder->height = height;
    encoder->full_chroma = full_chroma;
    for (int plane = 0; plane < 3; plane++)
        if (!(encoder->planes[plane] = malloc((size_t)width_of(encoder, plane) * rows_of(encoder, plane))))
            goto failed;

    enter();
    NV_ENC_PRESET_CONFIG preset = { .version = NV_ENC_PRESET_CONFIG_VER, .presetCfg.version = NV_ENC_CONFIG_VER };
    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS session = {
        .version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
        .deviceType = NV_ENC_DEVICE_TYPE_CUDA,
        .device = context,
        .apiVersion = NVENCAPI_VERSION,
    };
    /* HEVC where the client decodes it, the colours are reduced and the GPU encodes it; H.264 otherwise. */
    int opened = api.nvEncOpenEncodeSessionEx(&session, &encoder->session) == NV_ENC_SUCCESS;
    int hevc = opened && setup->hevc && !full_chroma
        && api.nvEncGetEncodePresetConfigEx(encoder->session, NV_ENC_CODEC_HEVC_GUID, PRESET, TUNING, &preset) == NV_ENC_SUCCESS;
    opened = opened
        && (hevc || api.nvEncGetEncodePresetConfigEx(encoder->session, NV_ENC_CODEC_H264_GUID, PRESET, TUNING, &preset) == NV_ENC_SUCCESS);
    if (opened) {
        NV_ENC_CONFIG *config = &encoder->config;
        *config = preset.presetCfg;
        config->gopLength = NVENC_INFINITE_GOPLENGTH;
        config->frameIntervalP = 1;
        config->rcParams.rateControlMode = NV_ENC_PARAMS_RC_VBR;
        config->rcParams.zeroReorderDelay = 1;
        set_rate(&config->rcParams, kbps, BUFFER_KBITS(kbps, setup->buffer), setup->quality);
        if (hevc)
            configure_hevc(config, width, height);
        else
            configure_h264(config, width, height, full_chroma);
        encoder->base.codec = hevc ? CODEC_HEVC : CODEC_H264;
        encoder->kept = hevc ? hevc_kept_frames(width, height) : h264_kept_frames(width, height);

        encoder->setup = (NV_ENC_INITIALIZE_PARAMS){
            .version = NV_ENC_INITIALIZE_PARAMS_VER,
            .encodeGUID = hevc ? NV_ENC_CODEC_HEVC_GUID : NV_ENC_CODEC_H264_GUID,
            .presetGUID = PRESET,
            .tuningInfo = TUNING,
            .encodeWidth = width,
            .encodeHeight = height,
            .darWidth = width,
            .darHeight = height,
            .frameRateNum = fps,
            .frameRateDen = 1,
            .enablePTD = 1, /* the encoder decides each picture's type */
            .encodeConfig = config,
        };
        NV_ENC_CREATE_INPUT_BUFFER input = {
            .version = NV_ENC_CREATE_INPUT_BUFFER_VER,
            .width = width,
            .height = height,
            .bufferFmt = full_chroma ? NV_ENC_BUFFER_FORMAT_YUV444 : NV_ENC_BUFFER_FORMAT_IYUV,
        };
        NV_ENC_CREATE_BITSTREAM_BUFFER output = { .version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER };
        opened = api.nvEncInitializeEncoder(encoder->session, &encoder->setup) == NV_ENC_SUCCESS
            && api.nvEncCreateInputBuffer(encoder->session, &input) == NV_ENC_SUCCESS
            && api.nvEncCreateBitstreamBuffer(encoder->session, &output) == NV_ENC_SUCCESS;
        encoder->input = input.inputBuffer;
        encoder->output = output.bitstreamBuffer;
    }
    leave();
    if (opened)
        return &encoder->base;
failed:
    nvenc_close(&encoder->base);
    return NULL;
}

static uint8_t *nvenc_plane(struct encoder *base, int index, int *stride)
{
    struct nvenc *encoder = (struct nvenc *)base;
    *stride = width_of(encoder, index);
    return encoder->planes[index];
}

static int nvenc_headers(struct encoder *base, uint8_t **sets, int *sizes)
{
    struct nvenc *encoder = (struct nvenc *)base;
    uint32_t size = 0;
    NV_ENC_SEQUENCE_PARAM_PAYLOAD payload = {
        .version = NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER,
        .inBufferSize = sizeof encoder->headers,
        .spsppsBuffer = encoder->headers,
        .outSPSPPSPayloadSize = &size,
    };
    enter();
    NVENCSTATUS status = api.nvEncGetSequenceParams(encoder->session, &payload);
    leave();
    if (status != NV_ENC_SUCCESS)
        return -1;
    int codec = encoder->base.codec;
    for (int code = nal_start_code(encoder->headers, size, 0), begin, next; code < (int)size; code = next) {
        int length = nal_unit(encoder->headers, size, code, &begin, &next);
        int type = length ? nal_type(encoder->headers + begin, codec) : 0;
        /* H.264's SPS and PPS, HEVC's VPS, SPS and PPS, in that order. */
        int at = codec == CODEC_HEVC ? (type == NAL_HEVC_VPS ? 0 : type == NAL_HEVC_SPS ? 1 : type == NAL_HEVC_PPS ? 2 : -1)
                                     : (type == NAL_SPS ? 0 : type == NAL_PPS ? 1 : -1);
        if (at >= 0) {
            sets[at] = encoder->headers + begin;
            sizes[at] = length;
        }
    }
    return codec == CODEC_HEVC ? 3 : 2;
}

/* Recovers by telling the driver that the frames after the one confirmed are lost; it refers to an older one
   it kept, or makes an intra frame. */
static int nvenc_encode(struct encoder *base, const struct encode_request *request, uint8_t **data, int *keyframe, int *flags)
{
    struct nvenc *encoder = (struct nvenc *)base;
    int size = -1;
    /* The frames kept: since the last keyframe, and no more than the buffer holds. */
    int64_t first = encoder->last - encoder->kept + 1;
    first = first > encoder->keyframe ? first : encoder->keyframe;
    int recover = request->kind == ENCODE_RECOVER && request->confirmed >= first;
    enter();
    for (int64_t lost = request->confirmed + 1; recover && lost <= encoder->last; lost++)
        api.nvEncInvalidateRefFrames(encoder->session, (uint64_t)lost);

    NV_ENC_LOCK_INPUT_BUFFER input = { .version = NV_ENC_LOCK_INPUT_BUFFER_VER, .inputBuffer = encoder->input };
    if (api.nvEncLockInputBuffer(encoder->session, &input) != NV_ENC_SUCCESS)
        goto done;
    /* The driver's buffer holds the planes one after another; its rows are `pitch` apart for the first plane
       and for full-size colour planes, and half that for half-size ones. */
    uint8_t *to = input.bufferDataPtr;
    for (int plane = 0; plane < 3; plane++) {
        int width = width_of(encoder, plane);
        int pitch = plane == 0 || encoder->full_chroma ? input.pitch : input.pitch / 2;
        for (int row = 0; row < rows_of(encoder, plane); row++, to += pitch)
            memcpy(to, encoder->planes[plane] + (size_t)row * width, width);
    }
    api.nvEncUnlockInputBuffer(encoder->session, encoder->input);

    NV_ENC_PIC_PARAMS picture = {
        .version = NV_ENC_PIC_PARAMS_VER,
        .inputWidth = encoder->width,
        .inputHeight = encoder->height,
        .inputPitch = input.pitch,
        .inputBuffer = encoder->input,
        .outputBitstream = encoder->output,
        .bufferFmt = encoder->full_chroma ? NV_ENC_BUFFER_FORMAT_YUV444 : NV_ENC_BUFFER_FORMAT_IYUV,
        .pictureStruct = NV_ENC_PIC_STRUCT_FRAME,
        .inputTimeStamp = (uint64_t)request->id,
        .encodePicFlags = request->kind != ENCODE_NEXT && !recover ? NV_ENC_PIC_FLAG_FORCEIDR : 0,
    };
    NV_ENC_LOCK_BITSTREAM stream = { .version = NV_ENC_LOCK_BITSTREAM_VER, .outputBitstream = encoder->output };
    /* Locking the output waits until the frame is encoded. */
    if (api.nvEncEncodePicture(encoder->session, &picture) != NV_ENC_SUCCESS || api.nvEncLockBitstream(encoder->session, &stream) != NV_ENC_SUCCESS)
        goto done;
    size = nal_length_prefixed(&encoder->unit, stream.bitstreamBufferPtr, stream.bitstreamSizeInBytes, encoder->base.codec);
    *data = encoder->unit.bytes;
    *keyframe = stream.pictureType == NV_ENC_PIC_TYPE_IDR;
    /* Whether the cap decided how good the frame is, this encoder does not say: its quality target is not a
       quantiser to compare with. A quantiser this coarse is far from any target. */
    *flags = stream.frameAvgQP > POOR_QP ? FRAME_POOR : 0;
    *flags |= recover && !*keyframe ? FRAME_RECOVERED : 0;
    if (*keyframe)
        encoder->keyframe = request->id;
    encoder->last = request->id;
    api.nvEncUnlockBitstream(encoder->session, encoder->output);
done:
    leave();
    return size;
}

static int nvenc_set_rate(struct encoder *base, int kbps, int buffer_kbits, int quality)
{
    struct nvenc *encoder = (struct nvenc *)base;
    NV_ENC_RC_PARAMS before = encoder->config.rcParams;
    set_rate(&encoder->config.rcParams, kbps, buffer_kbits, quality);
    NV_ENC_RECONFIGURE_PARAMS change = { .version = NV_ENC_RECONFIGURE_PARAMS_VER, .reInitEncodeParams = encoder->setup };
    enter();
    NVENCSTATUS status = api.nvEncReconfigureEncoder(encoder->session, &change);
    leave();
    if (status != NV_ENC_SUCCESS)
        encoder->config.rcParams = before;
    return status == NV_ENC_SUCCESS ? 0 : -1;
}

/* Pictures come in its planes only. */
static const struct encoder_ops nvenc_ops = {
    "nvenc", nvenc_plane, nvenc_headers, nvenc_encode, nvenc_set_rate, nvenc_close, NULL, NULL,
};
