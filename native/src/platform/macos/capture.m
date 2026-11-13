/* The picture of one display, through ScreenCaptureKit, for capture.rs. Only the newest new frame is kept;
   frames that say nothing changed are dropped, so a still screen sends nothing. */
#import <ScreenCaptureKit/ScreenCaptureKit.h>

/* How long the stream may take to start or stop. */
#define WAIT_SECONDS 5

@interface Capture : NSObject <SCStreamOutput, SCStreamDelegate>
@property(nonatomic, strong) SCStream *stream;
@property(nonatomic, strong) NSCondition *arrived;
@property(nonatomic) CVPixelBufferRef latest;
@property(nonatomic) BOOL stopped;
@end

@implementation Capture
- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sample ofType:(SCStreamOutputType)type
{
    CFArrayRef attachments = CMSampleBufferGetSampleAttachmentsArray(sample, false);
    if (!attachments || CFArrayGetCount(attachments) == 0)
        return;
    NSDictionary *info = (__bridge NSDictionary *)CFArrayGetValueAtIndex(attachments, 0);
    CVPixelBufferRef picture = CMSampleBufferGetImageBuffer(sample);
    if ([info[SCStreamFrameInfoStatus] integerValue] != SCFrameStatusComplete || !picture)
        return;
    [self.arrived lock];
    CVPixelBufferRelease(self.latest);
    self.latest = CVPixelBufferRetain(picture);
    [self.arrived signal];
    [self.arrived unlock];
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error
{
    [self.arrived lock];
    self.stopped = YES;
    [self.arrived signal];
    [self.arrived unlock];
}
@end

/* Waits for a block that takes a completion handler of one NSError, and returns that error. */
static NSError *wait_for(void (^call)(void (^)(NSError *)))
{
    __block NSError *failed = [NSError errorWithDomain:@"remote-desktop" code:0 userInfo:@{ NSLocalizedDescriptionKey : @"no answer" }];
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    call(^(NSError *error) {
        failed = error;
        dispatch_semaphore_signal(done);
    });
    dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, WAIT_SECONDS * NSEC_PER_SEC));
    return failed;
}

static void say(char *error, int size, NSError *failed)
{
    snprintf(error, size, "%s", failed.localizedDescription.UTF8String);
}

/* Starts capturing `display` at its pixel size, at most `fps` frames a second, with the pointer if `cursor`. */
void *capture_start(uint32_t display, int fps, int cursor, char *error, int error_size)
{
    __block SCShareableContent *content = nil;
    NSError *failed = wait_for(^(void (^done)(NSError *)) {
        [SCShareableContent getShareableContentWithCompletionHandler:^(SCShareableContent *found, NSError *problem) {
            content = found;
            done(problem);
        }];
    });
    if (failed) {
        say(error, error_size, failed);
        return NULL;
    }
    SCDisplay *shown = nil;
    for (SCDisplay *candidate in content.displays)
        if (candidate.displayID == display)
            shown = candidate;
    if (!shown) {
        snprintf(error, error_size, "the display is gone");
        return NULL;
    }

    SCStreamConfiguration *configuration = [[SCStreamConfiguration alloc] init];
    CGDisplayModeRef mode = CGDisplayCopyDisplayMode(display);
    configuration.width = CGDisplayModeGetPixelWidth(mode);
    configuration.height = CGDisplayModeGetPixelHeight(mode);
    CGDisplayModeRelease(mode);
    configuration.pixelFormat = kCVPixelFormatType_32BGRA;
    configuration.minimumFrameInterval = CMTimeMake(1, fps);
    configuration.showsCursor = cursor;

    Capture *capture = [[Capture alloc] init];
    capture.arrived = [[NSCondition alloc] init];
    SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:shown excludingWindows:@[]];
    capture.stream = [[SCStream alloc] initWithFilter:filter configuration:configuration delegate:capture];
    dispatch_queue_t queue = dispatch_queue_create(NULL, DISPATCH_QUEUE_SERIAL);
    [capture.stream addStreamOutput:capture type:SCStreamOutputTypeScreen sampleHandlerQueue:queue error:&failed];
    if (!failed)
        failed = wait_for(^(void (^done)(NSError *)) { [capture.stream startCaptureWithCompletionHandler:done]; });
    if (failed) {
        say(error, error_size, failed);
        return NULL;
    }
    return (__bridge_retained void *)capture;
}

/* Waits up to `millis` for a new frame. 1 with it locked for reading as BGRA rows, 0 if none came, -1 if the
   stream stopped. */
int capture_next(void *handle, int millis, void **frame, const uint8_t **pixels, size_t *stride, size_t *width, size_t *height)
{
    Capture *capture = (__bridge Capture *)handle;
    NSDate *until = [NSDate dateWithTimeIntervalSinceNow:millis / 1000.0];
    [capture.arrived lock];
    while (!capture.latest && !capture.stopped && [capture.arrived waitUntilDate:until])
        ;
    CVPixelBufferRef buffer = capture.latest;
    capture.latest = NULL;
    BOOL stopped = capture.stopped;
    [capture.arrived unlock];
    if (!buffer)
        return stopped ? -1 : 0;
    CVPixelBufferLockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly);
    *frame = buffer;
    *pixels = CVPixelBufferGetBaseAddress(buffer);
    *stride = CVPixelBufferGetBytesPerRow(buffer);
    *width = CVPixelBufferGetWidth(buffer);
    *height = CVPixelBufferGetHeight(buffer);
    return 1;
}

void capture_release(void *frame)
{
    CVPixelBufferUnlockBaseAddress(frame, kCVPixelBufferLock_ReadOnly);
    CVPixelBufferRelease(frame);
}

void capture_stop(void *handle)
{
    Capture *capture = (__bridge_transfer Capture *)handle;
    wait_for(^(void (^done)(NSError *)) { [capture.stream stopCaptureWithCompletionHandler:done]; });
    CVPixelBufferRelease(capture.latest);
    capture.stream = nil;
}
