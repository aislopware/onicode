#import "OniSourceTextRun.h"
#import <React/RCTFabricComponentsPlugins.h>
#import "OniSourceTextRunComponentDescriptor.h"
#import <react/renderer/components/OniSourceTextSpec/Props.h>
#import <react/renderer/components/OniSourceTextSpec/RCTComponentViewHelpers.h>

using namespace facebook::react;

@interface OniSourceTextRun () <RCTT3SourceTextRunViewProtocol>

@end

@implementation OniSourceTextRun {
  NSString *_text;
}

+ (ComponentDescriptorProvider)componentDescriptorProvider {
  return concreteComponentDescriptorProvider<
      OniSourceTextRunComponentDescriptor>();
}

- (instancetype)initWithFrame:(CGRect)frame {
  if (self = [super initWithFrame:frame]) {
    static const auto defaultProps =
        std::make_shared<const OniSourceTextRunProps>();
    _props = defaultProps;
  }
  return self;
}

- (void)updateProps:(Props::Shared const &)props
           oldProps:(Props::Shared const &)oldProps {
  const auto &oldViewProps =
      *std::static_pointer_cast<OniSourceTextRunProps const>(_props);
  const auto &newViewProps =
      *std::static_pointer_cast<OniSourceTextRunProps const>(props);

  if (newViewProps.text != oldViewProps.text) {
    NSString *text = [NSString stringWithUTF8String:newViewProps.text.c_str()];
    _text = text;
  }

  [super updateProps:props oldProps:oldProps];
}

+ (BOOL)shouldBeRecycled {
  return NO;
}

Class<RCTComponentViewProtocol> OniSourceTextRunCls(void) {
  return OniSourceTextRun.class;
}

@end
