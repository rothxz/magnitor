// EAS Build's configuration for Magnitor Remote (Exact's recipes/eas-ios.md, eas-android.md). Exact reads
// app.json; this file is EAS's alone and returns only Expo's configuration.
module.exports = {
  name: 'Magnitor Remote',
  slug: 'magnitor-remote',
  owner: 'roth-dev',
  version: process.env.EXACT_VERSION || '1.0.0',
  ios: { bundleIdentifier: 'com.magnitor.remote' },
  android: { package: 'com.magnitor.remote' },
  extra: { eas: { projectId: '06ac35a1-ca43-4040-ad6c-667f20ae8c3d' } },
};
