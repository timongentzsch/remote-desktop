/* The Mac's sound, for audio.rs: ScreenCaptureKit records what the Mac plays (but this program's own) at 48 kHz
   in two channels, as floats, from macOS 13 on; it is handed on as interleaved 16-bit samples. The stream needs
   a display to go with it, whose picture, two pixels at a frame a second, is let go. */
#import <ScreenCaptureKit/ScreenCaptureKit.h>

#define RATE 48000
#define CHANNELS 2

API_AVAILABLE(macos(13.0))
@interface Sound : NSObject <SCStreamOutput, SCStreamDelegate>
@property(nonatomic, strong) SCStream *stream;
@property(nonatomic, strong) NSCondition *arrived;
@property(nonatomic, strong) NSMutableData *samples;
@property(nonatomic) BOOL stopped;
@end

static int16_t to_integer(float sample)
{
    float clipped = sample > 1 ? 1 : sample < -1 ? -1 : sample;
    return (int16_t)(clipped * 32767);
}

@implementation Sound
- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sample ofType:(SCStreamOutputType)type
{
    if (type != SCStreamOutputTypeAudio)
        return;
    const AudioStreamBasicDescription *format = CMAudioFormatDescriptionGetStreamBasicDescription(CMSampleBufferGetFormatDescription(sample));
    if (!format || !(format->mFormatFlags & kAudioFormatFlagIsFloat) || format->mBitsPerChannel != 32)
        return;
    struct {
        AudioBufferList list;
        AudioBuffer more[CHANNELS - 1];
    } buffers;
    CMBlockBufferRef block = NULL;
    if (CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sample, NULL, &buffers.list, sizeof buffers, NULL, NULL, 0, &block) != noErr)
        return;
    CMItemCount frames = CMSampleBufferGetNumSamples(sample);
    int separate = (format->mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0;
    int16_t *interleaved = malloc(frames * CHANNELS * sizeof(int16_t));
    for (CMItemCount frame = 0; frame < frames; frame++)
        for (int channel = 0; channel < CHANNELS; channel++) {
            /* A single channel is played on both. */
            int from = channel < (int)format->mChannelsPerFrame ? channel : 0;
            const float *data = separate ? buffers.list.mBuffers[from].mData : buffers.list.mBuffers[0].mData;
            float value = separate ? data[frame] : data[frame * format->mChannelsPerFrame + from];
            interleaved[frame * CHANNELS + channel] = to_integer(value);
        }
    CFRelease(block);
    [self.arrived lock];
    [self.samples appendBytes:interleaved length:frames * CHANNELS * sizeof(int16_t)];
    [self.arrived signal];
    [self.arrived unlock];
    free(interleaved);
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error
{
    [self.arrived lock];
    self.stopped = YES;
    [self.arrived signal];
    [self.arrived unlock];
}
@end

/* In capture.m. */
NSError *wait_for(void (^call)(void (^)(NSError *)));

static void *start(char *error, int error_size) API_AVAILABLE(macos(13.0))
{
    __block SCShareableContent *content = nil;
    NSError *failed = wait_for(^(void (^done)(NSError *)) {
        [SCShareableContent getShareableContentWithCompletionHandler:^(SCShareableContent *found, NSError *problem) {
            content = found;
            done(problem);
        }];
    });
    if (!failed && content.displays.count == 0)
        failed = [NSError errorWithDomain:@"remote-desktop" code:0 userInfo:@{ NSLocalizedDescriptionKey : @"there is no display" }];
    if (failed) {
        snprintf(error, error_size, "%s", failed.localizedDescription.UTF8String);
        return NULL;
    }
    SCStreamConfiguration *configuration = [[SCStreamConfiguration alloc] init];
    configuration.capturesAudio = YES;
    configuration.excludesCurrentProcessAudio = YES;
    configuration.sampleRate = RATE;
    configuration.channelCount = CHANNELS;
    configuration.width = 2;
    configuration.height = 2;
    configuration.minimumFrameInterval = CMTimeMake(1, 1);

    Sound *sound = [[Sound alloc] init];
    sound.arrived = [[NSCondition alloc] init];
    sound.samples = [NSMutableData data];
    SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:content.displays.firstObject excludingWindows:@[]];
    sound.stream = [[SCStream alloc] initWithFilter:filter configuration:configuration delegate:sound];
    dispatch_queue_t queue = dispatch_queue_create(NULL, DISPATCH_QUEUE_SERIAL);
    [sound.stream addStreamOutput:sound type:SCStreamOutputTypeAudio sampleHandlerQueue:queue error:&failed];
    if (!failed)
        [sound.stream addStreamOutput:sound type:SCStreamOutputTypeScreen sampleHandlerQueue:queue error:&failed];
    if (!failed)
        failed = wait_for(^(void (^done)(NSError *)) { [sound.stream startCaptureWithCompletionHandler:done]; });
    if (failed) {
        snprintf(error, error_size, "%s", failed.localizedDescription.UTF8String);
        return NULL;
    }
    return (__bridge_retained void *)sound;
}

/* Starts recording. NULL, with what went wrong in `error`, where it cannot. */
void *sound_start(char *error, int error_size)
{
    if (@available(macOS 13.0, *))
        return start(error, error_size);
    snprintf(error, error_size, "macOS records its sound for others from version 13 on");
    return NULL;
}

/* Waits up to `millis` for samples, and moves up to `capacity` of them, interleaved, into `out`. Returns how
   many, or -1 if the stream stopped. */
int sound_take(void *handle, int16_t *out, int capacity, int millis) API_AVAILABLE(macos(13.0))
{
    Sound *sound = (__bridge Sound *)handle;
    NSDate *until = [NSDate dateWithTimeIntervalSinceNow:millis / 1000.0];
    [sound.arrived lock];
    while (sound.samples.length == 0 && !sound.stopped && [sound.arrived waitUntilDate:until])
        ;
    int count = (int)(sound.samples.length / sizeof(int16_t));
    count = count < capacity ? count : capacity;
    memcpy(out, sound.samples.bytes, count * sizeof(int16_t));
    [sound.samples replaceBytesInRange:NSMakeRange(0, count * sizeof(int16_t)) withBytes:NULL length:0];
    BOOL stopped = sound.stopped;
    [sound.arrived unlock];
    return count == 0 && stopped ? -1 : count;
}

void sound_stop(void *handle) API_AVAILABLE(macos(13.0))
{
    Sound *sound = (__bridge_transfer Sound *)handle;
    wait_for(^(void (^done)(NSError *)) { [sound.stream stopCaptureWithCompletionHandler:done]; });
    sound.stream = nil;
}
