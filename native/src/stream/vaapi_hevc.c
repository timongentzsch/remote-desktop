/* HEVC through VA-API (see vaapi.c): Main profile, 4:2:0, each frame referring to the one before it, or for a
   client that lost frames to one it has, of up to MOST_KEPT older ones every frame keeps. The
   block sizes and asymmetric motion partitions are those the driver says it encodes with, as ffmpeg takes
   them; where it says nothing, as Intel's does for its Coffee Lake GPUs, those Intel's GPUs encode with: 32x32
   coding tree blocks, coding blocks from 8x8, asymmetric motion partitions. SAO and temporal motion vector
   prediction are off. */
#ifdef __linux__

#include "vaapi.h"

/* Main profile at level 5.0, the main tier: pictures of up to 8912896 luma samples, 4096x2176. */
#define PROFILE_MAIN 1
#define LEVEL 150
#define MOST_SAMPLES 8912896
/* The blocks where the driver does not say, in log2 of pixels. */
#define LOG2_CTB 5
#define LOG2_MIN_CB 3
#define LOG2_MIN_TB 2
#define LOG2_MAX_TB 5
#define TRANSFORM_DEPTH 3
/* The coded picture's size is a multiple of this at least, as ffmpeg makes it. The standard asks only for a
   multiple of the smallest coding block, but Intel's driver encodes nothing for a picture 2168 lines high. */
#define LEAST_ALIGNMENT 16
/* The picture order count's low bits, 12 of them, go in each slice header. */
#define LOG2_MAX_POC_LSB 12
#define MERGE_CANDIDATES 5
/* Slice types (table 7-7), coding types as VA-API numbers them, and NAL unit types of pictures. */
#define SLICE_P 1
#define SLICE_I 2
#define CODING_I 1
#define CODING_P 2
#define NAL_TRAIL_R 1
#define NAL_IDR_W_RADL 19
/* No collocated picture: temporal motion vector prediction is off. */
#define NO_COLLOCATED 0xFF

static int round_up(int length, int multiple)
{
    return (length + multiple - 1) / multiple * multiple;
}

static int largest(int a, int b, int c)
{
    return a > b ? (a > c ? a : c) : (b > c ? b : c);
}

/* Takes the blocks and tools the driver says it encodes with, and from them the coded picture's size: a
   multiple of the smallest coding block, of what the driver asks surfaces to be and of LEAST_ALIGNMENT. */
static void configure(struct vaapi *encoder)
{
    struct hevc_blocks *hevc = &encoder->hevc;
    *hevc = (struct hevc_blocks){ LOG2_CTB, LOG2_MIN_CB, LOG2_MIN_TB, LOG2_MAX_TB, TRANSFORM_DEPTH, TRANSFORM_DEPTH, 1, 0, 0 };
    VAConfigAttribValEncHEVCBlockSizes sizes = { .value = vaapi_attribute(encoder, VAConfigAttribEncHEVCBlockSizes) };
    if (sizes.value != VA_ATTRIB_NOT_SUPPORTED) {
        hevc->log2_ctb = sizes.bits.log2_max_coding_tree_block_size_minus3 + 3;
        hevc->log2_min_cb = sizes.bits.log2_min_luma_coding_block_size_minus3 + 3;
        hevc->log2_min_tb = sizes.bits.log2_min_luma_transform_block_size_minus2 + 2;
        hevc->log2_max_tb = sizes.bits.log2_max_luma_transform_block_size_minus2 + 2;
        hevc->depth_inter = sizes.bits.max_max_transform_hierarchy_depth_inter;
        hevc->depth_intra = sizes.bits.max_max_transform_hierarchy_depth_intra;
    }
    VAConfigAttribValEncHEVCFeatures features = { .value = vaapi_attribute(encoder, VAConfigAttribEncHEVCFeatures) };
    if (features.value != VA_ATTRIB_NOT_SUPPORTED)
        hevc->amp = features.bits.amp != VA_FEATURE_NOT_SUPPORTED;
    hevc->width = round_up(encoder->width, largest(LEAST_ALIGNMENT, 1 << hevc->log2_min_cb, encoder->align_width));
    hevc->height = round_up(encoder->height, largest(LEAST_ALIGNMENT, 1 << hevc->log2_min_cb, encoder->align_height));
    encoder->block = 1 << hevc->log2_ctb;
    encoder->width_blocks = round_up(hevc->width, encoder->block) / encoder->block;
    encoder->height_blocks = round_up(hevc->height, encoder->block) / encoder->block;
}

/* The part of the coded picture that is not shown, right and below, in pairs of pixels as 4:2:0 counts it. */
static int crop_right(const struct vaapi *encoder)
{
    return (encoder->hevc.width - encoder->width) / 2;
}

static int crop_bottom(const struct vaapi *encoder)
{
    return (encoder->hevc.height - encoder->height) / 2;
}

/* profile_tier_level, as the VPS and SPS both carry it: progressive frames only. */
static void write_profile(struct bits *bits)
{
    bits_put(bits, 0, 2);            /* general_profile_space */
    bits_put(bits, 0, 1);            /* general_tier_flag: main */
    bits_put(bits, PROFILE_MAIN, 5); /* general_profile_idc */
    bits_put(bits, 0x60000000, 32);  /* general_profile_compatibility_flag[1] and [2]: Main, and Main 10 */
    bits_put(bits, 1, 1);            /* general_progressive_source_flag */
    bits_put(bits, 0, 1);            /* general_interlaced_source_flag */
    bits_put(bits, 1, 1);            /* general_non_packed_constraint_flag */
    bits_put(bits, 1, 1);            /* general_frame_only_constraint_flag */
    bits_put(bits, 0, 32);           /* the reserved and other constraint flags, 43 bits, */
    bits_put(bits, 0, 11);
    bits_put(bits, 0, 1); /* and general_inbld_flag */
    bits_put(bits, LEVEL, 8);
}

/* A decoder holds the picture being decoded, the one it refers to and those kept, and shows each at once. At
   level 5.0 it holds at least six pictures of the largest size. */
static void write_ordering(struct bits *bits)
{
    bits_put(bits, 0, 1);         /* sub_layer_ordering_info_present_flag */
    bits_put_ue(bits, 1 + MOST_KEPT); /* max_dec_pic_buffering_minus1 */
    bits_put_ue(bits, 0); /* max_num_reorder_pics */
    bits_put_ue(bits, 0); /* max_latency_increase_plus1 */
}

static void write_vps(struct vaapi *encoder)
{
    struct bits vps = { 0 };
    bits_put(&vps, 0, 4);      /* vps_video_parameter_set_id */
    bits_put(&vps, 1, 1);      /* vps_base_layer_internal_flag */
    bits_put(&vps, 1, 1);      /* vps_base_layer_available_flag */
    bits_put(&vps, 0, 6);      /* vps_max_layers_minus1 */
    bits_put(&vps, 0, 3);      /* vps_max_sub_layers_minus1 */
    bits_put(&vps, 1, 1);      /* vps_temporal_id_nesting_flag */
    bits_put(&vps, 0xFFFF, 16); /* vps_reserved_0xffff_16bits */
    write_profile(&vps);
    write_ordering(&vps);
    bits_put(&vps, 0, 6); /* vps_max_layer_id */
    bits_put_ue(&vps, 0); /* vps_num_layer_sets_minus1 */
    bits_put(&vps, 0, 1); /* vps_timing_info_present_flag */
    bits_put(&vps, 0, 1); /* vps_extension_flag */
    bits_trailing(&vps);
    vaapi_header(encoder, NAL_HEVC_VPS, &vps);
}

/* The sequence parameter set: the coded size and the part that is shown, the blocks and tools, and how its
   colours are meant (as encoder.h says). Its VUI says that frames come in the order they are shown. */
static void write_sps(struct vaapi *encoder)
{
    struct bits sps = { 0 };
    bits_put(&sps, 0, 4); /* sps_video_parameter_set_id */
    bits_put(&sps, 0, 3); /* sps_max_sub_layers_minus1 */
    bits_put(&sps, 1, 1); /* sps_temporal_id_nesting_flag */
    write_profile(&sps);
    bits_put_ue(&sps, 0); /* sps_seq_parameter_set_id */
    bits_put_ue(&sps, 1); /* chroma_format_idc: 4:2:0 */
    bits_put_ue(&sps, encoder->hevc.width);
    bits_put_ue(&sps, encoder->hevc.height);
    int cropped = crop_right(encoder) || crop_bottom(encoder);
    bits_put(&sps, cropped, 1); /* conformance_window_flag */
    if (cropped) {
        bits_put_ue(&sps, 0);
        bits_put_ue(&sps, crop_right(encoder));
        bits_put_ue(&sps, 0);
        bits_put_ue(&sps, crop_bottom(encoder));
    }
    bits_put_ue(&sps, 0); /* bit_depth_luma_minus8 */
    bits_put_ue(&sps, 0); /* bit_depth_chroma_minus8 */
    bits_put_ue(&sps, LOG2_MAX_POC_LSB - 4);
    write_ordering(&sps);
    bits_put_ue(&sps, encoder->hevc.log2_min_cb - 3);
    bits_put_ue(&sps, encoder->hevc.log2_ctb - encoder->hevc.log2_min_cb);
    bits_put_ue(&sps, encoder->hevc.log2_min_tb - 2);
    bits_put_ue(&sps, encoder->hevc.log2_max_tb - encoder->hevc.log2_min_tb);
    bits_put_ue(&sps, encoder->hevc.depth_inter); /* max_transform_hierarchy_depth_inter */
    bits_put_ue(&sps, encoder->hevc.depth_intra); /* max_transform_hierarchy_depth_intra */
    bits_put(&sps, 0, 1);                         /* scaling_list_enabled_flag */
    bits_put(&sps, encoder->hevc.amp, 1);         /* amp_enabled_flag */
    bits_put(&sps, 0, 1);               /* sample_adaptive_offset_enabled_flag */
    bits_put(&sps, 0, 1);               /* pcm_enabled_flag */
    bits_put_ue(&sps, 0);               /* num_short_term_ref_pic_sets: each slice says its own */
    bits_put(&sps, 0, 1);               /* long_term_ref_pics_present_flag */
    bits_put(&sps, 0, 1);               /* sps_temporal_mvp_enabled_flag */
    bits_put(&sps, 0, 1);               /* strong_intra_smoothing_enabled_flag */
    bits_put(&sps, 1, 1);               /* vui_parameters_present_flag */
    bits_put(&sps, 0, 1);               /* aspect_ratio_info_present_flag */
    bits_put(&sps, 0, 1);               /* overscan_info_present_flag */
    bits_put(&sps, 1, 1);               /* video_signal_type_present_flag */
    bits_put(&sps, 5, 3);               /* video_format: unspecified */
    bits_put(&sps, 1, 1);               /* video_full_range_flag */
    bits_put(&sps, 1, 1);               /* colour_description_present_flag */
    bits_put(&sps, BT709, 8);
    bits_put(&sps, BT709, 8);
    bits_put(&sps, BT709, 8);
    bits_put(&sps, 1, 1); /* chroma_loc_info_present_flag */
    bits_put_ue(&sps, CHROMA_AT_CENTRE);
    bits_put_ue(&sps, CHROMA_AT_CENTRE);
    bits_put(&sps, 0, 1);                   /* neutral_chroma_indication_flag */
    bits_put(&sps, 0, 1);                   /* field_seq_flag */
    bits_put(&sps, 0, 1);                   /* frame_field_info_present_flag */
    bits_put(&sps, 0, 1);                   /* default_display_window_flag */
    bits_put(&sps, 1, 1);                   /* vui_timing_info_present_flag */
    bits_put(&sps, 1, 32);                  /* vui_num_units_in_tick */
    bits_put(&sps, encoder->base.fps, 32); /* vui_time_scale */
    bits_put(&sps, 0, 1);                   /* vui_poc_proportional_to_timing_flag: frames come when they change */
    bits_put(&sps, 0, 1);                   /* vui_hrd_parameters_present_flag */
    bits_put(&sps, 1, 1);                   /* bitstream_restriction_flag */
    bits_put(&sps, 0, 1);                   /* tiles_fixed_structure_flag */
    bits_put(&sps, 1, 1);                   /* motion_vectors_over_pic_boundaries_flag */
    bits_put(&sps, 1, 1);                   /* restricted_ref_pic_lists_flag */
    bits_put_ue(&sps, 0);                   /* min_spatial_segmentation_idc */
    bits_put_ue(&sps, 0);                   /* max_bytes_per_pic_denom */
    bits_put_ue(&sps, 0);                   /* max_bits_per_min_cu_denom */
    bits_put_ue(&sps, 15);                  /* log2_max_mv_length_horizontal */
    bits_put_ue(&sps, 15);                  /* log2_max_mv_length_vertical */
    bits_put(&sps, 0, 1);                   /* sps_extension_present_flag */
    bits_trailing(&sps);
    vaapi_header(encoder, NAL_HEVC_SPS, &sps);
}

/* The picture parameter set: one reference, loop filtering across slices, nothing else switched on. */
static void write_pps(struct vaapi *encoder)
{
    struct bits pps = { 0 };
    bits_put_ue(&pps, 0); /* pps_pic_parameter_set_id */
    bits_put_ue(&pps, 0); /* pps_seq_parameter_set_id */
    bits_put(&pps, 0, 1); /* dependent_slice_segments_enabled_flag */
    bits_put(&pps, 0, 1); /* output_flag_present_flag */
    bits_put(&pps, 0, 3); /* num_extra_slice_header_bits */
    bits_put(&pps, 0, 1); /* sign_data_hiding_enabled_flag */
    bits_put(&pps, 0, 1); /* cabac_init_present_flag */
    bits_put_ue(&pps, 0); /* num_ref_idx_l0_default_active_minus1 */
    bits_put_ue(&pps, 0); /* num_ref_idx_l1_default_active_minus1 */
    bits_put_se(&pps, INITIAL_QP - 26);
    bits_put(&pps, 0, 1); /* constrained_intra_pred_flag */
    bits_put(&pps, 0, 1); /* transform_skip_enabled_flag */
    bits_put(&pps, 0, 1); /* cu_qp_delta_enabled_flag */
    bits_put_se(&pps, 0); /* pps_cb_qp_offset */
    bits_put_se(&pps, 0); /* pps_cr_qp_offset */
    bits_put(&pps, 0, 1); /* pps_slice_chroma_qp_offsets_present_flag */
    bits_put(&pps, 0, 1); /* weighted_pred_flag */
    bits_put(&pps, 0, 1); /* weighted_bipred_flag */
    bits_put(&pps, 0, 1); /* transquant_bypass_enabled_flag */
    bits_put(&pps, 0, 1); /* tiles_enabled_flag */
    bits_put(&pps, 0, 1); /* entropy_coding_sync_enabled_flag */
    bits_put(&pps, 1, 1); /* pps_loop_filter_across_slices_enabled_flag */
    bits_put(&pps, 0, 1); /* deblocking_filter_control_present_flag */
    bits_put(&pps, 0, 1); /* pps_scaling_list_data_present_flag */
    bits_put(&pps, 0, 1); /* lists_modification_present_flag */
    bits_put_ue(&pps, 0); /* log2_parallel_merge_level_minus2 */
    bits_put(&pps, 0, 1); /* slice_segment_header_extension_present_flag */
    bits_put(&pps, 0, 1); /* pps_extension_present_flag */
    bits_trailing(&pps);
    vaapi_header(encoder, NAL_HEVC_PPS, &pps);
}

static void write_headers(struct vaapi *encoder)
{
    write_vps(encoder);
    write_sps(encoder);
    write_pps(encoder);
}

/* The sequence as the driver is told it: as the SPS says. */
static int add_sequence(struct vaapi *encoder, VABufferID *buffers, int *count)
{
    VAEncSequenceParameterBufferHEVC sequence = {
        .general_profile_idc = PROFILE_MAIN,
        .general_level_idc = LEVEL,
        .intra_period = NO_PERIOD,
        .intra_idr_period = NO_PERIOD,
        .ip_period = 1,
        .pic_width_in_luma_samples = encoder->hevc.width,
        .pic_height_in_luma_samples = encoder->hevc.height,
        .seq_fields.bits = {
            .chroma_format_idc = 1,
            .amp_enabled_flag = encoder->hevc.amp,
        },
        .log2_min_luma_coding_block_size_minus3 = encoder->hevc.log2_min_cb - 3,
        .log2_diff_max_min_luma_coding_block_size = encoder->hevc.log2_ctb - encoder->hevc.log2_min_cb,
        .log2_min_transform_block_size_minus2 = encoder->hevc.log2_min_tb - 2,
        .log2_diff_max_min_transform_block_size = encoder->hevc.log2_max_tb - encoder->hevc.log2_min_tb,
        .max_transform_hierarchy_depth_inter = encoder->hevc.depth_inter,
        .max_transform_hierarchy_depth_intra = encoder->hevc.depth_intra,
        /* The VUI is in the SPS as it is written; the driver is not told it, as ffmpeg does not tell it. */
    };
    return vaapi_add(encoder, VAEncSequenceParameterBufferType, &sequence, sizeof sequence, buffers, count);
}

/* The slice segment header of a frame's one slice, as the SPS and PPS have it: an IDR's, or a P slice's that
   refers to encoder->references[0] and keeps the others, at quantiser `qp`. */
static int add_slice_header(struct vaapi *encoder, int idr, int qp, VABufferID *buffers, int *count)
{
    struct bits header = { 0 };
    bits_put(&header, 1, 1); /* first_slice_segment_in_pic_flag */
    if (idr)
        bits_put(&header, 0, 1); /* no_output_of_prior_pics_flag */
    bits_put_ue(&header, 0);     /* slice_pic_parameter_set_id */
    bits_put_ue(&header, idr ? SLICE_I : SLICE_P);
    if (!idr) {
        bits_put(&header, encoder->frames % (1u << LOG2_MAX_POC_LSB), LOG2_MAX_POC_LSB);
        bits_put(&header, 0, 1); /* short_term_ref_pic_set_sps_flag: the set follows */
        bits_put_ue(&header, encoder->reference_count); /* num_negative_pics: newest first */
        bits_put_ue(&header, 0);                         /* num_positive_pics */
        uint32_t before = encoder->frames;
        for (int at = 0; at < encoder->reference_count; at++) {
            bits_put_ue(&header, before - encoder->references[at].order - 1); /* delta_poc_s0_minus1 */
            bits_put(&header, at == 0, 1);                                     /* used_by_curr_pic_s0_flag */
            before = encoder->references[at].order;
        }
        bits_put(&header, 0, 1); /* num_ref_idx_active_override_flag */
        bits_put_ue(&header, 5 - MERGE_CANDIDATES); /* five_minus_max_num_merge_cand */
    }
    bits_put_se(&header, qp - INITIAL_QP); /* slice_qp_delta */
    bits_put(&header, 0, 1);                /* slice_loop_filter_across_slices_enabled_flag */
    bits_trailing(&header);                 /* byte_alignment(): a one, then zeros */
    uint8_t nal[BITS_ROOM * 2];
    int size = nal_write(nal, sizeof nal, CODEC_HEVC, idr ? NAL_IDR_W_RADL : NAL_TRAIL_R, &header);
    return size < 0 ? -1 : vaapi_add_packed(encoder, VAEncPackedHeaderSlice, nal, size, buffers, count);
}

/* A picture entry that refers to nothing. */
static VAPictureHEVC nothing(void)
{
    return (VAPictureHEVC){ .picture_id = VA_INVALID_SURFACE, .flags = VA_PICTURE_HEVC_INVALID };
}

/* A frame's picture and its one slice. Its order is the frames since the IDR. */
static int add_frame(struct vaapi *encoder, int idr, int current, int qp, VABufferID *buffers, int *count)
{
    VAEncPictureParameterBufferHEVC picture = {
        .decoded_curr_pic = { .picture_id = encoder->surfaces[current], .pic_order_cnt = (int32_t)encoder->frames },
        .coded_buf = encoder->coded,
        .collocated_ref_pic_index = NO_COLLOCATED,
        .pic_init_qp = INITIAL_QP,
        .nal_unit_type = idr ? NAL_IDR_W_RADL : NAL_TRAIL_R,
        .pic_fields.bits = {
            .idr_pic_flag = idr,
            .coding_type = idr ? CODING_I : CODING_P,
            .reference_pic_flag = 1,
            .pps_loop_filter_across_slices_enabled_flag = 1,
        },
    };
    VAEncSliceParameterBufferHEVC slice = {
        .num_ctu_in_slice = encoder->width_blocks * encoder->height_blocks,
        .slice_type = idr ? SLICE_I : SLICE_P,
        .max_num_merge_cand = MERGE_CANDIDATES,
        .slice_qp_delta = qp - INITIAL_QP,
        .slice_fields.bits = { .last_slice_of_pic_flag = 1 },
    };
    for (int at = 0; at < 15; at++)
        picture.reference_frames[at] = slice.ref_pic_list0[at] = slice.ref_pic_list1[at] = nothing();
    /* Every frame kept is told, as ffmpeg tells them; the slice refers to the first. */
    for (int at = 0; at < encoder->reference_count && !idr; at++)
        picture.reference_frames[at] = (VAPictureHEVC){
            .picture_id = encoder->surfaces[encoder->references[at].surface],
            .pic_order_cnt = (int32_t)encoder->references[at].order,
            .flags = VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE,
        };
    if (!idr)
        slice.ref_pic_list0[0] = picture.reference_frames[0];
    if (vaapi_add(encoder, VAEncPictureParameterBufferType, &picture, sizeof picture, buffers, count) < 0
        || add_slice_header(encoder, idr, qp, buffers, count) < 0)
        return -1;
    return vaapi_add(encoder, VAEncSliceParameterBufferType, &slice, sizeof slice, buffers, count);
}

const struct vaapi_codec vaapi_hevc = {
    .codec = CODEC_HEVC,
    .profile = VAProfileHEVCMain,
    .block = 1 << LOG2_CTB,
    .most_samples = MOST_SAMPLES,
    .kept = MOST_KEPT,
    .configure = configure,
    /* Intel's driver writes no slice headers of its own: without them, encoding fails. */
    .packed_headers = VA_ENC_PACKED_HEADER_SEQUENCE | VA_ENC_PACKED_HEADER_SLICE,
    .write_headers = write_headers,
    .add_sequence = add_sequence,
    .add_frame = add_frame,
};

#endif
