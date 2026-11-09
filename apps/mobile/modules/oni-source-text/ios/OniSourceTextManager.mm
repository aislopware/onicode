#import <React/RCTUIManager.h>
#import <React/RCTViewManager.h>

@interface OniSourceTextManager : RCTViewManager
@end

@implementation OniSourceTextManager

RCT_EXPORT_MODULE(OniSourceText)

- (UIView *)view {
  return [[UIView alloc] init];
}

@end

@interface OniSourceTextRunManager : RCTViewManager
@end

@implementation OniSourceTextRunManager

RCT_EXPORT_MODULE(OniSourceTextRun)

- (UIView *)view {
  return nil;
}

@end
