/* H.264 through VA-API (see vaapi.c): High profile, 4:2:0, each frame referring to the one before it. */
#ifdef __linux__

#include "vaapi.h"

/* Level 5.2: pictures of up to 36864 macroblocks, 4096x2304. */
#define LEVEL 52
#define MACROBLOCK 16
/* The frame number counts up to 2^16 before it wraps; frames have nothing to say with it but their order. */
#define LOG2_MAX_FRAME_NUM 16
/* Slice types (table 7-6). */
#define SLICE_P 0
#define SLICE_I 2
#define PROFILE_HIGH 100

/* The part of the macroblocks that is not shown, right and below, in pairs of pixels as 4:2:0 counts it. */
static int crop_right(const struct vaapi *encoder)
{
    return (encoder->width_blocks * MACROBLOCK - encoder->width) / 2;
}

static int crop_bottom(const struct vaapi *encoder)
{
    return (encoder->height_blocks * MACROBLOCK - encoder->height) / 2;
}

/* The sequence parameter set: the picture's size and the part of its macroblocks that is shown, frames
   referring to one before them, and how its colours are meant (as encoder.h says). Its VUI says that frames
   come in the order they are shown, so that a decoder shows each one at once. */
static void write_sps(struct vaapi *encoder)
{
    struct bits sps = { 0 };
    bits_put(&sps, PROFILE_HIGH, 8);
    bits_put(&sps, 0, 8); /* constraint flags */
    bits_put(&sps, LEVEL, 8);
    bits_put_ue(&sps, 0); /* seq_parameter_set_id */
    bits_put_ue(&sps, 1); /* chroma_format_idc: 4:2:0 */
    bits_put_ue(&sps, 0); /* bit_depth_luma_minus8 */
    bits_put_ue(&sps, 0); /* bit_depth_chroma_minus8 */
    bits_put(&sps, 0, 1); /* qpprime_y_zero_transform_bypass_flag */
    bits_put(&sps, 0, 1); /* seq_scaling_matrix_present_flag */
    bits_put_ue(&sps, LOG2_MAX_FRAME_NUM - 4);
    bits_put_ue(&sps, 2); /* pic_order_cnt_type: in the order of frame_num */
    bits_put_ue(&sps, 1); /* max_num_ref_frames */
    bits_put(&sps, 0, 1); /* gaps_in_frame_num_value_allowed_flag */
    bits_put_ue(&sps, encoder->width_blocks - 1);
    bits_put_ue(&sps, encoder->height_blocks - 1);
    bits_put(&sps, 1, 1); /* frame_mbs_only_flag */
    bits_put(&sps, 1, 1); /* direct_8x8_inference_flag */
    int cropped = crop_right(encoder) || crop_bottom(encoder);
    bits_put(&sps, cropped, 1);
    if (cropped) {
        bits_put_ue(&sps, 0);
        bits_put_ue(&sps, crop_right(encoder));
        bits_put_ue(&sps, 0);
        bits_put_ue(&sps, crop_bottom(encoder));
    }
    bits_put(&sps, 1, 1); /* vui_parameters_present_flag */
    bits_put(&sps, 0, 1); /* aspect_ratio_info_present_flag */
    bits_put(&sps, 0, 1); /* overscan_info_present_flag */
    bits_put(&sps, 1, 1); /* video_signal_type_present_flag */
    bits_put(&sps, 5, 3); /* video_format: unspecified */
    bits_put(&sps, 1, 1); /* video_full_range_flag */
    bits_put(&sps, 1, 1); /* colour_description_present_flag */
    bits_put(&sps, BT709, 8);
    bits_put(&sps, BT709, 8);
    bits_put(&sps, BT709, 8);
    bits_put(&sps, 1, 1); /* chroma_loc_info_present_flag */
    bits_put_ue(&sps, CHROMA_AT_CENTRE);
    bits_put_ue(&sps, CHROMA_AT_CENTRE);
    bits_put(&sps, 1, 1);                       /* timing_info_present_flag */
    bits_put(&sps, 1, 32);                      /* num_units_in_tick */
    bits_put(&sps, 2 * encoder->base.fps, 32); /* time_scale: two ticks a frame */
    bits_put(&sps, 0, 1);                       /* fixed_frame_rate_flag */
    bits_put(&sps, 0, 1);                       /* nal_hrd_parameters_present_flag */
    bits_put(&sps, 0, 1);                       /* vcl_hrd_parameters_present_flag */
    bits_put(&sps, 0, 1);                       /* pic_struct_present_flag */
    bits_put(&sps, 1, 1);                       /* bitstream_restriction_flag */
    bits_put(&sps, 1, 1);                       /* motion_vectors_over_pic_boundaries_flag */
    bits_put_ue(&sps, 0);                       /* max_bytes_per_pic_denom */
    bits_put_ue(&sps, 0);                       /* max_bits_per_mb_denom */
    bits_put_ue(&sps, 15);                      /* log2_max_mv_length_horizontal */
    bits_put_ue(&sps, 15);                      /* log2_max_mv_length_vertical */
    bits_put_ue(&sps, 0);                       /* max_num_reorder_frames */
    bits_put_ue(&sps, 1);                       /* max_dec_frame_buffering */
    bits_trailing(&sps);
    vaapi_header(encoder, NAL_SPS, &sps);
}

/* The picture parameter set: CABAC, one reference, 8x8 transforms, and deblocking the slices may adjust. */
static void write_pps(struct vaapi *encoder)
{
    struct bits pps = { 0 };
    bits_put_ue(&pps, 0); /* pic_parameter_set_id */
    bits_put_ue(&pps, 0); /* seq_parameter_set_id */
    bits_put(&pps, 1, 1); /* entropy_coding_mode_flag: CABAC */
    bits_put(&pps, 0, 1); /* bottom_field_pic_order_in_frame_present_flag */
    bits_put_ue(&pps, 0); /* num_slice_groups_minus1 */
    bits_put_ue(&pps, 0); /* num_ref_idx_l0_default_active_minus1 */
    bits_put_ue(&pps, 0); /* num_ref_idx_l1_default_active_minus1 */
    bits_put(&pps, 0, 1); /* weighted_pred_flag */
    bits_put(&pps, 0, 2); /* weighted_bipred_idc */
    bits_put_se(&pps, INITIAL_QP - 26);
    bits_put_se(&pps, 0); /* pic_init_qs_minus26 */
    bits_put_se(&pps, 0); /* chroma_qp_index_offset */
    bits_put(&pps, 1, 1); /* deblocking_filter_control_present_flag */
    bits_put(&pps, 0, 1); /* constrained_intra_pred_flag */
    bits_put(&pps, 0, 1); /* redundant_pic_cnt_present_flag */
    bits_put(&pps, 1, 1); /* transform_8x8_mode_flag */
    bits_put(&pps, 0, 1); /* pic_scaling_matrix_present_flag */
    bits_put_se(&pps, 0); /* second_chroma_qp_index_offset */
    bits_trailing(&pps);
    vaapi_header(encoder, NAL_PPS, &pps);
}

static void write_headers(struct vaapi *encoder)
{
    write_sps(encoder);
    write_pps(encoder);
}

/* The sequence as the driver is told it: as the SPS says. */
static int add_sequence(struct vaapi *encoder, VABufferID *buffers, int *count)
{
    VAEncSequenceParameterBufferH264 sequence = {
        .level_idc = LEVEL,
        .intra_period = NO_PERIOD,
        .intra_idr_period = NO_PERIOD,
        .ip_period = 1,
        .max_num_ref_frames = 1,
        .picture_width_in_mbs = encoder->width_blocks,
        .picture_height_in_mbs = encoder->height_blocks,
        .seq_fields.bits = {
            .chroma_format_idc = 1,
            .frame_mbs_only_flag = 1,
            .direct_8x8_inference_flag = 1,
            .log2_max_frame_num_minus4 = LOG2_MAX_FRAME_NUM - 4,
            .pic_order_cnt_type = 2,
        },
        .frame_cropping_flag = crop_right(encoder) || crop_bottom(encoder),
        .frame_crop_right_offset = crop_right(encoder),
        .frame_crop_bottom_offset = crop_bottom(encoder),
        .vui_parameters_present_flag = 1,
        .vui_fields.bits = {
            .timing_info_present_flag = 1,
            .bitstream_restriction_flag = 1,
            .log2_max_mv_length_horizontal = 15,
            .log2_max_mv_length_vertical = 15,
            .motion_vectors_over_pic_boundaries_flag = 1,
        },
        .num_units_in_tick = 1,
        .time_scale = 2 * encoder->base.fps,
    };
    return vaapi_add(encoder, VAEncSequenceParameterBufferType, &sequence, sizeof sequence, buffers, count);
}

/* A picture entry that refers to nothing. */
static VAPictureH264 nothing(void)
{
    return (VAPictureH264){ .picture_id = VA_INVALID_SURFACE, .flags = VA_PICTURE_H264_INVALID };
}

/* A frame's picture and its one slice. Its frame number and its order are the frames since the IDR. */
static int add_frame(struct vaapi *encoder, int idr, int current, int qp, VABufferID *buffers, int *count)
{
    uint32_t number = encoder->frames % (1u << LOG2_MAX_FRAME_NUM);
    VAEncPictureParameterBufferH264 picture = {
        .CurrPic = {
            .picture_id = encoder->surfaces[current],
            .frame_idx = number,
            .TopFieldOrderCnt = 2 * encoder->frames,
            .BottomFieldOrderCnt = 2 * encoder->frames,
        },
        .coded_buf = encoder->coded,
        .frame_num = number,
        .pic_init_qp = INITIAL_QP,
        .pic_fields.bits = {
            .idr_pic_flag = idr,
            .reference_pic_flag = 1,
            .entropy_coding_mode_flag = 1,
            .transform_8x8_mode_flag = 1,
            .deblocking_filter_control_present_flag = 1,
        },
    };
    VAEncSliceParameterBufferH264 slice = {
        .num_macroblocks = encoder->width_blocks * encoder->height_blocks,
        .slice_type = idr ? SLICE_I : SLICE_P,
        .idr_pic_id = encoder->idr_id,
        .direct_spatial_mv_pred_flag = 1,
        .slice_qp_delta = qp - INITIAL_QP,
    };
    for (int at = 0; at < 16; at++)
        picture.ReferenceFrames[at] = nothing();
    for (int at = 0; at < 32; at++)
        slice.RefPicList0[at] = slice.RefPicList1[at] = nothing();
    if (!idr) {
        VAPictureH264 before = {
            .picture_id = encoder->surfaces[encoder->reference],
            .frame_idx = (encoder->frames - 1) % (1u << LOG2_MAX_FRAME_NUM),
            .flags = VA_PICTURE_H264_SHORT_TERM_REFERENCE,
            .TopFieldOrderCnt = 2 * (encoder->frames - 1),
            .BottomFieldOrderCnt = 2 * (encoder->frames - 1),
        };
        picture.ReferenceFrames[0] = slice.RefPicList0[0] = before;
    }
    if (vaapi_add(encoder, VAEncPictureParameterBufferType, &picture, sizeof picture, buffers, count) < 0)
        return -1;
    return vaapi_add(encoder, VAEncSliceParameterBufferType, &slice, sizeof slice, buffers, count);
}

const struct vaapi_codec vaapi_h264 = {
    .codec = CODEC_H264,
    .profile = VAProfileH264High,
    .block = MACROBLOCK,
    .most_samples = 36864 * MACROBLOCK * MACROBLOCK,
    .packed_headers = VA_ENC_PACKED_HEADER_SEQUENCE,
    .write_headers = write_headers,
    .add_sequence = add_sequence,
    .add_frame = add_frame,
};

#endif
