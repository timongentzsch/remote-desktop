/* The Mac's clipboard, for clipboard.rs: its text, and how many times it changed. */
#import <AppKit/AppKit.h>

long pasteboard_changes(void)
{
    return [NSPasteboard generalPasteboard].changeCount;
}

/* Copies the clipboard's text as UTF-8 into `out`. Returns its length, or -1 where there is none or it does not
   fit in `capacity` bytes. */
long pasteboard_read(char *out, long capacity)
{
    NSString *text = [[NSPasteboard generalPasteboard] stringForType:NSPasteboardTypeString];
    NSData *bytes = [text dataUsingEncoding:NSUTF8StringEncoding];
    if (!bytes || (long)bytes.length > capacity)
        return -1;
    memcpy(out, bytes.bytes, bytes.length);
    return (long)bytes.length;
}

void pasteboard_write(const char *text, long length)
{
    NSString *string = [[NSString alloc] initWithBytes:text length:length encoding:NSUTF8StringEncoding];
    if (!string)
        return;
    NSPasteboard *board = [NSPasteboard generalPasteboard];
    [board clearContents];
    [board setString:string forType:NSPasteboardTypeString];
}
