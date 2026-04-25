import { Stack } from 'expo-router';
import { UIBridgeNativeProvider } from '@qontinui/ui-bridge-native';

export default function RootLayout() {
  return (
    <UIBridgeNativeProvider
      features={{ server: __DEV__, debug: __DEV__ }}
      config={{
        serverPort: 8087,
        appInfo: {
          appId: 'expo-router-integrated',
          appName: 'Expo Router Integrated',
          appType: 'mobile',
          framework: 'expo',
        },
      }}
    >
      <Stack />
    </UIBridgeNativeProvider>
  );
}
