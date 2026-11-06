import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_hbb/common/widgets/setting_widgets.dart';
import 'package:flutter_hbb/desktop/pages/desktop_setting_page.dart';
import 'package:get/get.dart';
import 'package:provider/provider.dart';
import 'package:settings_ui/settings_ui.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:url_launcher/url_launcher_string.dart';

import '../../common.dart';
import '../../common/widgets/dialog.dart';
import '../../common/widgets/login.dart';
import '../../consts.dart';
import '../../models/model.dart';
import '../../models/platform_model.dart';
import '../widgets/dialog.dart';
import 'home_page.dart';

class SettingsPage extends StatefulWidget implements PageShape {
  @override
  final title = translate("Settings");

  @override
  final icon = Icon(Icons.settings);

  @override
  final appBarActions = [];

  @override
  State<SettingsPage> createState() => _SettingsState();
}

const url = 'https://www.hoptodesk.com/';

enum KeepScreenOn {
  never,
  duringControlled,
  serviceOn,
}

String _keepScreenOnToOption(KeepScreenOn value) {
  switch (value) {
    case KeepScreenOn.never:
      return 'never';
    case KeepScreenOn.duringControlled:
      return 'during-controlled';
    case KeepScreenOn.serviceOn:
      return 'service-on';
  }
}

KeepScreenOn optionToKeepScreenOn(String value) {
  switch (value) {
    case 'never':
      return KeepScreenOn.never;
    case 'service-on':
      return KeepScreenOn.serviceOn;
    default:
      return KeepScreenOn.duringControlled;
  }
}

class _SettingsState extends State<SettingsPage> with WidgetsBindingObserver {
  final _hasIgnoreBattery =
      false; //androidVersion >= 26; // remove because not work on every device
  var _ignoreBatteryOpt = false;
  var _enableStartOnBoot = false;
  var _dashboardLinking = false;
  var _floatingWindowDisabled = false;
  var _keepScreenOn = KeepScreenOn.duringControlled; // relay on floating window
  var _enableAbr = false;
  var _denyLANDiscovery = false;
  var _onlyWhiteList = false;
  var _enableDirectIPAccess = false;
  var _enableRecordSession = false;
  var _enableHardwareCodec = false;
  var _autoRecordIncomingSession = false;
  var _autoRecordOutgoingSession = false;
  var _allowAutoDisconnect = false;
  var _localIP = "";
  var _directAccessPort = "";
  var _fingerprint = "";
  var _buildDate = "";
  var _autoDisconnectTimeout = "";
  var _hideServer = false;
  var _hideProxy = false;
  var _hideNetwork = false;
  var _enableTrustedDevices = false;
  var _useHopToDeskNetwork = true;
  var _customApiUrl = "";
  var _localNetworkHost = "";

  _SettingsState() {
    _enableAbr = option2bool(
        kOptionEnableAbr, bind.mainGetOptionSync(key: kOptionEnableAbr));
    _denyLANDiscovery = !option2bool(kOptionEnableLanDiscovery,
        bind.mainGetOptionSync(key: kOptionEnableLanDiscovery));
    _onlyWhiteList = whitelistNotEmpty();
    _enableDirectIPAccess = option2bool(
        kOptionDirectServer, bind.mainGetOptionSync(key: kOptionDirectServer));
    _enableRecordSession = option2bool(kOptionEnableRecordSession,
        bind.mainGetOptionSync(key: kOptionEnableRecordSession));
    _enableHardwareCodec = option2bool(kOptionEnableHwcodec,
        bind.mainGetOptionSync(key: kOptionEnableHwcodec));
    _autoRecordIncomingSession = option2bool(kOptionAllowAutoRecordIncoming,
        bind.mainGetOptionSync(key: kOptionAllowAutoRecordIncoming));
    _autoRecordOutgoingSession = option2bool(kOptionAllowAutoRecordOutgoing,
        bind.mainGetLocalOption(key: kOptionAllowAutoRecordOutgoing));
    _localIP = bind.mainGetOptionSync(key: 'local-ip-addr');
    _directAccessPort = bind.mainGetOptionSync(key: kOptionDirectAccessPort);
    _allowAutoDisconnect = option2bool(kOptionAllowAutoDisconnect,
        bind.mainGetOptionSync(key: kOptionAllowAutoDisconnect));
    _autoDisconnectTimeout =
        bind.mainGetOptionSync(key: kOptionAutoDisconnectTimeout);
    _hideServer =
        bind.mainGetBuildinOption(key: kOptionHideServerSetting) == 'Y';
    _hideProxy = bind.mainGetBuildinOption(key: kOptionHideProxySetting) == 'Y';
    _hideNetwork =
        bind.mainGetBuildinOption(key: kOptionHideNetworkSetting) == 'Y';
    _enableTrustedDevices = mainGetBoolOptionSync(kOptionEnableTrustedDevices);
    _customApiUrl = bind.mainGetOptionSync(key: kOptionCustomApiUrl);
    _localNetworkHost = bind.mainGetLocalNetworkHost();
    _useHopToDeskNetwork = _customApiUrl.isEmpty && _localNetworkHost.isEmpty;
  }

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);

    WidgetsBinding.instance.addPostFrameCallback((_) async {
      var update = false;

      if (isAndroid) {
        if (_hasIgnoreBattery) {
          if (await checkAndUpdateIgnoreBatteryStatus()) {
            update = true;
          }
        }

        if (await checkAndUpdateStartOnBoot()) {
          update = true;
        }

        // start on boot depends on ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS and SYSTEM_ALERT_WINDOW
        var enableStartOnBoot =
            await gFFI.invokeMethod(AndroidChannel.kGetStartOnBootOpt);
        if (enableStartOnBoot) {
          if (!await canStartOnBoot()) {
            enableStartOnBoot = false;
            gFFI.invokeMethod(AndroidChannel.kSetStartOnBootOpt, false);
          }
        }

        if (enableStartOnBoot != _enableStartOnBoot) {
          update = true;
          _enableStartOnBoot = enableStartOnBoot;
        }

        var floatingWindowDisabled =
            bind.mainGetLocalOption(key: kOptionDisableFloatingWindow) == "Y" ||
                !await AndroidPermissionManager.check(kSystemAlertWindow);
        if (floatingWindowDisabled != _floatingWindowDisabled) {
          update = true;
          _floatingWindowDisabled = floatingWindowDisabled;
        }

        final keepScreenOn = _floatingWindowDisabled
            ? KeepScreenOn.never
            : optionToKeepScreenOn(
                bind.mainGetLocalOption(key: kOptionKeepScreenOn));
        if (keepScreenOn != _keepScreenOn) {
          update = true;
          _keepScreenOn = keepScreenOn;
        }
      }

      /*final fingerprint = await bind.mainGetFingerprint();
      if (_fingerprint != fingerprint) {
        update = true;
        _fingerprint = fingerprint;
      }*/

      final buildDate = await bind.mainGetBuildDate();
      if (_buildDate != buildDate) {
        update = true;
        _buildDate = buildDate;
      }

      if (update) {
        setState(() {});
      }
    });
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed && isAndroid) {
      () async {
        final ibs = await checkAndUpdateIgnoreBatteryStatus();
        final sob = await checkAndUpdateStartOnBoot();
        if (ibs || sob) {
          setState(() {});
        }
      }();
    }
  }

  Future<bool> checkAndUpdateIgnoreBatteryStatus() async {
    final res = await AndroidPermissionManager.check(
        kRequestIgnoreBatteryOptimizations);
    if (_ignoreBatteryOpt != res) {
      _ignoreBatteryOpt = res;
      return true;
    } else {
      return false;
    }
  }

  Future<bool> checkAndUpdateStartOnBoot() async {
    if (!await canStartOnBoot() && _enableStartOnBoot) {
      _enableStartOnBoot = false;
      debugPrint(
          "checkAndUpdateStartOnBoot and set _enableStartOnBoot -> false");
      gFFI.invokeMethod(AndroidChannel.kSetStartOnBootOpt, false);
      return true;
    } else {
      return false;
    }
  }

  @override
  Widget build(BuildContext context) {
    Provider.of<FfiModel>(context);
    final outgoingOnly = bind.isOutgoingOnly();
    final incommingOnly = bind.isIncomingOnly();
    final customClientSection = CustomSettingsSection(
        child: Align(
      alignment: Alignment.center,
      child: loadLogo(),
    ));
    final List<AbstractSettingsTile> enhancementsTiles = [];
    final enable2fa = bind.mainHasValid2FaSync();
    final List<AbstractSettingsTile> tfaTiles = [
      SettingsTile.switchTile(
        title: Text(translate('enable-2fa-title')),
        initialValue: enable2fa,
        onToggle: (v) async {
          update() async {
            setState(() {});
          }

          if (v == false) {
            CommonConfirmDialog(
                gFFI.dialogManager, translate('cancel-2fa-confirm-tip'), () {
              change2fa(callback: update);
            });
          } else {
            change2fa(callback: update);
          }
        },
      ),
      if (enable2fa)
        SettingsTile.switchTile(
          title: Text(translate('Telegram bot')),
          initialValue: bind.mainHasValidBotSync(),
          onToggle: (v) async {
            update() async {
              setState(() {});
            }

            if (v == false) {
              CommonConfirmDialog(
                  gFFI.dialogManager, translate('cancel-bot-confirm-tip'), () {
                changeBot(callback: update);
              });
            } else {
              changeBot(callback: update);
            }
          },
        ),
      if (enable2fa)
        SettingsTile.switchTile(
          title: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(translate('Enable trusted devices')),
              Text('* ${translate('enable-trusted-devices-tip')}',
                  style: Theme.of(context).textTheme.bodySmall),
            ],
          ),
          initialValue: _enableTrustedDevices,
          onToggle: isOptionFixed(kOptionEnableTrustedDevices)
              ? null
              : (v) async {
                  mainSetBoolOption(kOptionEnableTrustedDevices, v);
                  setState(() {
                    _enableTrustedDevices = v;
                  });
                },
        ),
      if (enable2fa && _enableTrustedDevices)
        SettingsTile(
            title: Text(translate('Manage trusted devices')),
            trailing: Icon(Icons.arrow_forward_ios),
            onPressed: (context) {
              Navigator.push(context, MaterialPageRoute(builder: (context) {
                return _ManageTrustedDevices();
              }));
            })
    ];
    final List<AbstractSettingsTile> shareScreenTiles = [
      SettingsTile.switchTile(
        title: Text(translate('Deny LAN Discovery')),
        initialValue: _denyLANDiscovery,
        onToggle: isOptionFixed(kOptionEnableLanDiscovery)
            ? null
            : (v) async {
                await bind.mainSetOption(
                    key: kOptionEnableLanDiscovery,
                    value: bool2option(kOptionEnableLanDiscovery, !v));
                final newValue = !option2bool(kOptionEnableLanDiscovery,
                    await bind.mainGetOption(key: kOptionEnableLanDiscovery));
                setState(() {
                  _denyLANDiscovery = newValue;
                });
              },
      ),
/*      SettingsTile.switchTile(
        title: Row(children: [
          Expanded(child: Text(translate('Use IP Whitelisting'))),
          Offstage(
                  offstage: !_onlyWhiteList,
                  child: const Icon(Icons.warning_amber_rounded,
                      color: Color.fromARGB(255, 255, 204, 0)))
              .marginOnly(left: 5)
        ]),
        initialValue: _onlyWhiteList,
        onToggle: (_) async {
          update() async {
            final onlyWhiteList = whitelistNotEmpty();
            if (onlyWhiteList != _onlyWhiteList) {
              setState(() {
                _onlyWhiteList = onlyWhiteList;
              });
            }
          }

          changeWhiteList(callback: update);
        },
      ),
      SettingsTile.switchTile(
        title: Text('${translate('Adaptive bitrate')} (beta)'),
        initialValue: _enableAbr,
        onToggle: isOptionFixed(kOptionEnableAbr)
            ? null
            : (v) async {
                await mainSetBoolOption(kOptionEnableAbr, v);
                final newValue = await mainGetBoolOption(kOptionEnableAbr);
                setState(() {
                  _enableAbr = newValue;
                });
              },
      ),
*/	  
      SettingsTile.switchTile(
        title: Text(translate('Enable Recording Session')),
        initialValue: _enableRecordSession,
        onToggle: isOptionFixed(kOptionEnableRecordSession)
            ? null
            : (v) async {
                await mainSetBoolOption(kOptionEnableRecordSession, v);
                final newValue =
                    await mainGetBoolOption(kOptionEnableRecordSession);
                setState(() {
                  _enableRecordSession = newValue;
                });
              },
      ),
      SettingsTile.switchTile(
        title: Row(
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              Expanded(
                  child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                    Text(translate("Direct IP Access")),
                    Offstage(
                        offstage: !_enableDirectIPAccess,
                        child: Text(
                          '${translate("Local Address")}: $_localIP${_directAccessPort.isEmpty ? "" : ":$_directAccessPort"}',
                          style: Theme.of(context).textTheme.bodySmall,
                        )),
                  ])),
              Offstage(
                  offstage: !_enableDirectIPAccess,
                  child: IconButton(
                      padding: EdgeInsets.zero,
                      icon: Icon(
                        Icons.edit,
                        size: 20,
                      ),
                      onPressed: isOptionFixed(kOptionDirectAccessPort)
                          ? null
                          : () async {
                              final port = await changeDirectAccessPort(
                                  _localIP, _directAccessPort);
                              setState(() {
                                _directAccessPort = port;
                              });
                            }))
            ]),
        initialValue: _enableDirectIPAccess,
        onToggle: isOptionFixed(kOptionDirectServer)
            ? null
            : (_) async {
                _enableDirectIPAccess = !_enableDirectIPAccess;
                String value =
                    bool2option(kOptionDirectServer, _enableDirectIPAccess);
                await bind.mainSetOption(
                    key: kOptionDirectServer, value: value);
                setState(() {});
              },
      ),
      SettingsTile.switchTile(
        title: Row(
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              Expanded(
                  child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                    Text(translate("auto_disconnect_option_tip")),
                    Offstage(
                        offstage: !_allowAutoDisconnect,
                        child: Text(
                          '${_autoDisconnectTimeout.isEmpty ? '10' : _autoDisconnectTimeout} min',
                          style: Theme.of(context).textTheme.bodySmall,
                        )),
                  ])),
              Offstage(
                  offstage: !_allowAutoDisconnect,
                  child: IconButton(
                      padding: EdgeInsets.zero,
                      icon: Icon(
                        Icons.edit,
                        size: 20,
                      ),
                      onPressed: isOptionFixed(kOptionAutoDisconnectTimeout)
                          ? null
                          : () async {
                              final timeout = await changeAutoDisconnectTimeout(
                                  _autoDisconnectTimeout);
                              setState(() {
                                _autoDisconnectTimeout = timeout;
                              });
                            }))
            ]),
        initialValue: _allowAutoDisconnect,
        onToggle: isOptionFixed(kOptionAllowAutoDisconnect)
            ? null
            : (_) async {
                _allowAutoDisconnect = !_allowAutoDisconnect;
                String value = bool2option(
                    kOptionAllowAutoDisconnect, _allowAutoDisconnect);
                await bind.mainSetOption(
                    key: kOptionAllowAutoDisconnect, value: value);
                setState(() {});
              },
      )
    ];
    if (_hasIgnoreBattery) {
      enhancementsTiles.insert(
          0,
          SettingsTile.switchTile(
              initialValue: _ignoreBatteryOpt,
              title: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(translate('Keep HopToDesk background service')),
                    Text('* ${translate('Ignore Battery Optimizations')}',
                        style: Theme.of(context).textTheme.bodySmall),
                  ]),
              onToggle: (v) async {
                if (v) {
                  await AndroidPermissionManager.request(
                      kRequestIgnoreBatteryOptimizations);
                } else {
                  final res = await gFFI.dialogManager.show<bool>(
                      (setState, close, context) => CustomAlertDialog(
                            title: Text(translate("Open System Setting")),
                            content: Text(translate(
                                "android_open_battery_optimizations_tip")),
                            actions: [
                              dialogButton("Cancel",
                                  onPressed: () => close(), isOutline: true),
                              dialogButton(
                                "Open System Setting",
                                onPressed: () => close(true),
                              ),
                            ],
                          ));
                  if (res == true) {
                    AndroidPermissionManager.startAction(
                        kActionApplicationDetailsSettings);
                  }
                }
              }));
    }
    enhancementsTiles.add(SettingsTile.switchTile(
        initialValue: _enableStartOnBoot,
        title: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text("${translate('Start on Boot')}"),
          Text(
              '* ${translate('Start the screen sharing service on boot, requires special permissions')}',
              style: Theme.of(context).textTheme.bodySmall),
        ]),
        onToggle: (toValue) async {
          if (toValue) {
            // 1. request kIgnoreBatteryOptimizations
            if (!await AndroidPermissionManager.check(
                kRequestIgnoreBatteryOptimizations)) {
              if (!await AndroidPermissionManager.request(
                  kRequestIgnoreBatteryOptimizations)) {
                return;
              }
            }

            // 2. request kSystemAlertWindow
            if (!await AndroidPermissionManager.check(kSystemAlertWindow)) {
              if (!await AndroidPermissionManager.request(kSystemAlertWindow)) {
                return;
              }
            }

            // (Optional) 3. request input permission
          }
          setState(() => _enableStartOnBoot = toValue);

          gFFI.invokeMethod(AndroidChannel.kSetStartOnBootOpt, toValue);
        }));

    onFloatingWindowChanged(bool toValue) async {
      if (toValue) {
        if (!await AndroidPermissionManager.check(kSystemAlertWindow)) {
          if (!await AndroidPermissionManager.request(kSystemAlertWindow)) {
            return;
          }
        }
      }
      final disable = !toValue;
      bind.mainSetLocalOption(
          key: kOptionDisableFloatingWindow,
          value: disable ? 'Y' : defaultOptionNo);
      setState(() => _floatingWindowDisabled = disable);
      gFFI.serverModel.androidUpdatekeepScreenOn();
    }

    enhancementsTiles.add(SettingsTile.switchTile(
        initialValue: !_floatingWindowDisabled,
        title: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text(translate('Floating window')),
          Text('* ${translate('floating_window_tip')}',
              style: Theme.of(context).textTheme.bodySmall),
        ]),
        onToggle: bind.mainIsOptionFixed(key: kOptionDisableFloatingWindow)
            ? null
            : onFloatingWindowChanged));
//hophere
/*
    enhancementsTiles.add(_getPopupDialogRadioEntry(
      title: 'Keep screen on',
      list: [
        _RadioEntry('Never', _keepScreenOnToOption(KeepScreenOn.never)),
        _RadioEntry('During controlled',
            _keepScreenOnToOption(KeepScreenOn.duringControlled)),
        _RadioEntry('During service is on',
            _keepScreenOnToOption(KeepScreenOn.serviceOn)),
      ],
      getter: () => _keepScreenOnToOption(_floatingWindowDisabled
          ? KeepScreenOn.never
          : optionToKeepScreenOn(
              bind.mainGetLocalOption(key: kOptionKeepScreenOn))),
      asyncSetter: isOptionFixed(kOptionKeepScreenOn) || _floatingWindowDisabled
          ? null
          : (value) async {
              await bind.mainSetLocalOption(
                  key: kOptionKeepScreenOn, value: value);
              setState(() => _keepScreenOn = optionToKeepScreenOn(value));
              gFFI.serverModel.androidUpdatekeepScreenOn();
            },
    ));
*/
    final disabledSettings = bind.isDisableSettings();
    final hideSecuritySettings =
        bind.mainGetBuildinOption(key: kOptionHideSecuritySetting) == 'Y';
    final settings = SettingsList(
      sections: [
        if (isBrandedBuild) customClientSection,
        if (!bind.isDisableAccount())
          /*SettingsSection(
            title: Text(translate('Account')),
            tiles: [
              SettingsTile(
                title: Obx(() => Text(gFFI.userModel.userName.value.isEmpty
                    ? translate('Login')
                    : '${translate('Logout')} (${gFFI.userModel.userName.value})')),
                leading: Icon(Icons.person),
                onPressed: (context) {
                  if (gFFI.userModel.userName.value.isEmpty) {
                    loginDialog();
                  } else {
                    logOutConfirmDialog();
                  }
                },
              ),
            ],
          ),*/
        SettingsSection(title: Text(translate("Settings")), tiles: [
          if (!disabledSettings)
/*            SettingsTile(
                title: Text(translate('ID/Relay Server')),
                leading: Icon(Icons.cloud),
                onPressed: (context) {
                  showServerSettings(gFFI.dialogManager);
                }),*/
          SettingsTile(
              title: Text(translate('Language')),
              leading: Icon(Icons.translate),
              onPressed: (context) {
                showLanguageSettings(gFFI.dialogManager);
              }),
          SettingsTile(
            title: Text(translate(
                Theme.of(context).brightness == Brightness.light
                    ? 'Light Theme'
                    : 'Dark Theme')),
            leading: Icon(Theme.of(context).brightness == Brightness.light
                ? Icons.dark_mode
                : Icons.light_mode),
            onPressed: (context) {
              showThemeSettings(gFFI.dialogManager);
            },
          ),
          if (bind.mainGetBuildinOption(key: kOptionDisplayName).isEmpty)
            SettingsTile(
              title: Text(translate('Display Name')),
              leading: Icon(Icons.badge),
              value: Padding(
                padding: EdgeInsets.symmetric(vertical: 8),
                child: Text(displayNameLabel()),
              ),
              onPressed: (context) => showDisplayName(() {
                if (mounted) setState(() {});
              }),
            ),
          if (isAndroid || isIOS)
            SettingsTile(
              title: Text(translate('Dashboard')),
              leading: Icon(Icons.dashboard),
              value: Padding(
                padding: EdgeInsets.symmetric(vertical: 8),
                child: Text(dashboardStatusLabel(_dashboardLinking)),
              ),
              onPressed: (context) => showDashboardLink(
                refresh: () {
                  if (mounted) setState(() {});
                },
                setLinking: (v) {
                  if (mounted) setState(() => _dashboardLinking = v);
                },
              ),
            )
        ]),
        if (isAndroid)
          /*SettingsSection(title: Text(translate('Hardware Codec')), tiles: [
            SettingsTile.switchTile(
              title: Text(translate('Enable hardware codec')),
              initialValue: _enableHardwareCodec,
              onToggle: isOptionFixed(kOptionEnableHwcodec)
                  ? null
                  : (v) async {
                      await mainSetBoolOption(kOptionEnableHwcodec, v);
                      final newValue =
                          await mainGetBoolOption(kOptionEnableHwcodec);
                      setState(() {
                        _enableHardwareCodec = newValue;
                      });
                    },
            ),
          ]),*/
        if (isAndroid)
          SettingsSection(
            title: Text(translate("Recording")),
            tiles: [
              if (!outgoingOnly)
              SettingsTile.switchTile(
                title:
                    Text(translate('Automatically record incoming sessions')),
                initialValue: _autoRecordIncomingSession,
                onToggle: isOptionFixed(kOptionAllowAutoRecordIncoming)
                    ? null
                    : (v) async {
                        await bind.mainSetOption(
                            key: kOptionAllowAutoRecordIncoming,
                              value: bool2option(
                                  kOptionAllowAutoRecordIncoming, v));
                        final newValue = option2bool(
                            kOptionAllowAutoRecordIncoming,
                            await bind.mainGetOption(
                                key: kOptionAllowAutoRecordIncoming));
                        setState(() {
                          _autoRecordIncomingSession = newValue;
                        });
                      },
              ),
              if (!incommingOnly)
                SettingsTile.switchTile(
                  title:
                      Text(translate('Automatically record outgoing sessions')),
                  initialValue: _autoRecordOutgoingSession,
                  onToggle: isOptionFixed(kOptionAllowAutoRecordOutgoing)
                      ? null
                      : (v) async {
                          await bind.mainSetLocalOption(
                              key: kOptionAllowAutoRecordOutgoing,
                              value: bool2option(
                                  kOptionAllowAutoRecordOutgoing, v));
                          final newValue = option2bool(
                              kOptionAllowAutoRecordOutgoing,
                              bind.mainGetLocalOption(
                                  key: kOptionAllowAutoRecordOutgoing));
                          setState(() {
                            _autoRecordOutgoingSession = newValue;
                          });
                        },
                ),
              SettingsTile(
                title: Text(translate("Directory")),
                description: Text(bind.mainVideoSaveDirectory(root: false)),
              ),
            ],
          ),
        if (isAndroid &&
            !disabledSettings &&
            !outgoingOnly &&
            !hideSecuritySettings)
          SettingsSection(title: Text('2FA'), tiles: tfaTiles),
        if (isAndroid &&
            !disabledSettings &&
            !outgoingOnly &&
            !hideSecuritySettings)
          SettingsSection(
            title: Text(translate("Share Screen")),
            tiles: shareScreenTiles,
          ),
        if (!bind.isIncomingOnly()) defaultDisplaySection(),
        SettingsSection(
            title: Text(translate("Network")),
            tiles: [
              SettingsTile.switchTile(
                title: Row(
                    mainAxisAlignment: MainAxisAlignment.spaceBetween,
                    crossAxisAlignment: CrossAxisAlignment.center,
                    children: [
                      Expanded(
                          child: Column(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                            Text(translate(isBrandedBuild
                                ? "Use Default Network"
                                : "Use HopToDesk Network")),
                            Offstage(
                                offstage: _useHopToDeskNetwork,
                                child: Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    Text(
                                      translate(_localNetworkHost.isEmpty
                                          ? "Using Custom Network URL:"
                                          : "Custom Network Settings"),
                                      style:
                                          Theme.of(context).textTheme.bodySmall,
                                    ),
                                    Text(
                                      _localNetworkHost.isEmpty
                                          ? _customApiUrl
                                          : _localNetworkHost,
                                      style:
                                          Theme.of(context).textTheme.bodySmall,
                                    ),
                                  ],
                                )),
                          ])),
                      Offstage(
                          offstage: _useHopToDeskNetwork ||
                              _localNetworkHost.isNotEmpty,
                          child: IconButton(
                              padding: EdgeInsets.zero,
                              icon: Icon(
                                Icons.edit,
                                size: 20,
                              ),
                              onPressed: () async {
                                final url =
                                    await changeCustomApiUrl(_customApiUrl);
                                if (url != null) {
                                  setState(() {
                                    _customApiUrl = url;
                                    _useHopToDeskNetwork = url.isEmpty;
                                  });
                                }
                              }))
                    ]),
                initialValue: _useHopToDeskNetwork,
                enabled: _localNetworkHost.isEmpty,
                onToggle: (v) async {
                  if (v) {
                    // Toggling ON = use default HopToDesk Network
                    await bind.mainSetOption(
                        key: kOptionCustomApiUrl, value: '');
                    setState(() {
                      _useHopToDeskNetwork = true;
                      _customApiUrl = '';
                    });
                  } else {
                    // Toggling OFF = enter custom URL
                    final url = await changeCustomApiUrl('');
                    if (url != null && url.isNotEmpty) {
                      setState(() {
                        _customApiUrl = url;
                        _useHopToDeskNetwork = false;
                      });
                    } else {
                      // User cancelled, keep toggle ON
                      setState(() {});
                    }
                  }
                },
              ),
            ],
          ),
        if (isAndroid &&
            !disabledSettings &&
            !outgoingOnly &&
            !hideSecuritySettings)
          SettingsSection(
            title: Text(translate("Enhancements")),
            tiles: enhancementsTiles,
          ),
        SettingsSection(
          title: Text(translate("About")),
          tiles: [
            SettingsTile(
                onPressed: isIOS ? null : (context) async {
                  if (await canLaunchUrl(Uri.parse(url))) {
                    await launchUrl(Uri.parse(url));
                  }
                },
                title: Text(translate("Version: ") + version),
                value: isIOS ? null : Padding(
                  padding: EdgeInsets.symmetric(vertical: 8),
                  child: Text('hoptodesk.com',
                      style: TextStyle(
                        decoration: TextDecoration.underline,
                      )),
                ),
                leading: Icon(Icons.info)),
/*            
			SettingsTile(
                title: Text(translate("Build Date")),
                value: Padding(
                  padding: EdgeInsets.symmetric(vertical: 8),
                  child: Text(_buildDate),
                ),
                leading: Icon(Icons.query_builder)),
            if (isAndroid)
              SettingsTile(
                onPressed: (context) => onCopyFingerprint(_fingerprint),
                title: Text(translate("Fingerprint")),
                value: Padding(
                  padding: EdgeInsets.symmetric(vertical: 8),
                  child: Text(_fingerprint),
                ),
                leading: Icon(Icons.fingerprint)),
            SettingsTile(
              title: Text(translate("Privacy Statement")),
              onPressed: (context) =>
                  launchUrlString('https://rustdesk.com/privacy.html'),
              leading: Icon(Icons.privacy_tip),
            )
*/            
          ],
        ),
      ],
    );
    return settings;
  }

  Future<bool> canStartOnBoot() async {
    // start on boot depends on ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS and SYSTEM_ALERT_WINDOW
    if (_hasIgnoreBattery && !_ignoreBatteryOpt) {
      return false;
    }
    if (!await AndroidPermissionManager.check(kSystemAlertWindow)) {
      return false;
    }
    return true;
  }

  defaultDisplaySection() {
    return SettingsSection(
      title: Text(translate("Display Settings")),
      tiles: [
        SettingsTile(
            title: Text(translate('Display Settings')),
            leading: Icon(Icons.desktop_windows_outlined),
            trailing: Icon(Icons.arrow_forward_ios),
            onPressed: (context) {
              Navigator.push(context, MaterialPageRoute(builder: (context) {
                return _DisplayPage();
              }));
            })
      ],
    );
  }
}

void showServerSettings(OverlayDialogManager dialogManager) async {
  Map<String, dynamic> options = jsonDecode(await bind.mainGetOptions());
  showServerSettingsWithValue(ServerConfig.fromOptions(options), dialogManager);
}

void showLanguageSettings(OverlayDialogManager dialogManager) async {
  try {
    final langs = json.decode(await bind.mainGetLangs()) as List<dynamic>;
    var lang = bind.mainGetLocalOption(key: kCommConfKeyLang);
    dialogManager.show((setState, close, context) {
      setLang(v) async {
        if (lang != v) {
          setState(() {
            lang = v;
          });
          await bind.mainSetLocalOption(key: kCommConfKeyLang, value: v);
          HomePage.homeKey.currentState?.refreshPages();
          Future.delayed(Duration(milliseconds: 200), close);
        }
      }

      final isOptFixed = isOptionFixed(kCommConfKeyLang);
      return CustomAlertDialog(
        content: Column(
          children: [
                getRadio(Text(translate('Default')), defaultOptionLang, lang,
                    isOptFixed ? null : setLang),
                Divider(color: MyTheme.border),
              ] +
              langs.map((e) {
                final key = e[0] as String;
                final name = e[1] as String;
                return getRadio(Text(translate(name)), key, lang,
                    isOptFixed ? null : setLang);
              }).toList(),
        ),
      );
    }, backDismiss: true, clickMaskDismiss: true);
  } catch (e) {
    //
  }
}

void showThemeSettings(OverlayDialogManager dialogManager) async {
  var themeMode = MyTheme.getThemeModePreference();

  dialogManager.show((setState, close, context) {
    setTheme(v) {
      if (themeMode != v) {
        setState(() {
          themeMode = v;
        });
        MyTheme.changeDarkMode(themeMode);
        Future.delayed(Duration(milliseconds: 200), close);
      }
    }

    final isOptFixed = isOptionFixed(kCommConfKeyTheme);
    return CustomAlertDialog(
      content: Column(children: [
        getRadio(Text(translate('Light')), ThemeMode.light, themeMode,
            isOptFixed ? null : setTheme),
        getRadio(Text(translate('Dark')), ThemeMode.dark, themeMode,
            isOptFixed ? null : setTheme),
        getRadio(Text(translate('Follow System')), ThemeMode.system, themeMode,
            isOptFixed ? null : setTheme)
      ]),
    );
  }, backDismiss: true, clickMaskDismiss: true);
}

void showAbout(OverlayDialogManager dialogManager) {
  dialogManager.show((setState, close, context) {
    return CustomAlertDialog(
      title: Text('${translate('About')} $appName'),
      content: Wrap(direction: Axis.vertical, spacing: 12, children: [
        Text('Version: $version'),
        if (!isIOS)
        InkWell(
              onTap: () async {
                const url = 'https://www.hoptodesk.com/';
                if (await canLaunchUrl(Uri.parse(url))) {
                  await launchUrl(Uri.parse(url));
                }
              },
              child: Padding(
                padding: EdgeInsets.symmetric(vertical: 8),
                child: Text('hoptodesk.com',
                    style: TextStyle(
                      decoration: TextDecoration.underline,
                    )),
              )),
      ]),
      actions: [],
    );
  }, clickMaskDismiss: true, backDismiss: true);
}

/*
class ScanButton extends StatelessWidget {
  @override
  Widget build(BuildContext context) {
    return IconButton(
      icon: Icon(Icons.qr_code_scanner),
      onPressed: () {
        Navigator.push(
          context,
          MaterialPageRoute(
            builder: (BuildContext context) => ScanPage(),
          ),
        );
      },
    );
  }
}
*/

class _DisplayPage extends StatefulWidget {
  const _DisplayPage();

  @override
  State<_DisplayPage> createState() => __DisplayPageState();
}

class __DisplayPageState extends State<_DisplayPage> {
  @override
  Widget build(BuildContext context) {
    final Map codecsJson = jsonDecode(bind.mainSupportedHwdecodings());
    final h264 = codecsJson['h264'] ?? false;
    final h265 = codecsJson['h265'] ?? false;
    var codecList = [
      _RadioEntry('Auto', 'auto'),
      _RadioEntry('VP8', 'vp8'),
      _RadioEntry('VP9', 'vp9'),
      //_RadioEntry('AV1', 'av1'),
      if (h264) _RadioEntry('H264', 'h264'),
      if (h265) _RadioEntry('H265', 'h265')
    ];
    RxBool showCustomImageQuality = false.obs;
    return Scaffold(
      appBar: AppBar(
        leading: IconButton(
            onPressed: () => Navigator.pop(context),
            icon: Icon(Icons.arrow_back_ios)),
        title: Text(translate('Display Settings')),
        centerTitle: true,
      ),
      body: SettingsList(sections: [
        SettingsSection(
          tiles: [
            _getPopupDialogRadioEntry(
              title: 'Default View Style',
              list: [
                _RadioEntry('Scale original', kRemoteViewStyleOriginal),
                _RadioEntry('Scale adaptive', kRemoteViewStyleAdaptive)
              ],
              getter: () =>
                  bind.mainGetUserDefaultOption(key: kOptionViewStyle),
              asyncSetter: isOptionFixed(kOptionViewStyle)
                  ? null
                  : (value) async {
                      await bind.mainSetUserDefaultOption(
                          key: kOptionViewStyle, value: value);
                    },
            ),
            _getPopupDialogRadioEntry(
              title: 'Default Image Quality',
              list: [
                _RadioEntry('Good image quality', kRemoteImageQualityBest),
                _RadioEntry('Balanced', kRemoteImageQualityBalanced),
                _RadioEntry('Optimize reaction time', kRemoteImageQualityLow),
                _RadioEntry('Custom', kRemoteImageQualityCustom),
              ],
              getter: () {
                final v =
                    bind.mainGetUserDefaultOption(key: kOptionImageQuality);
                showCustomImageQuality.value = v == kRemoteImageQualityCustom;
                return v;
              },
              asyncSetter: isOptionFixed(kOptionImageQuality)
                  ? null
                  : (value) async {
                      await bind.mainSetUserDefaultOption(
                          key: kOptionImageQuality, value: value);
                      showCustomImageQuality.value =
                          value == kRemoteImageQualityCustom;
                    },
              tail: customImageQualitySetting(),
              showTail: showCustomImageQuality,
              notCloseValue: kRemoteImageQualityCustom,
            ),
            _getPopupDialogRadioEntry(
              title: 'Default Codec',
              list: codecList,
              getter: () =>
                  bind.mainGetUserDefaultOption(key: kOptionCodecPreference),
              asyncSetter: isOptionFixed(kOptionCodecPreference)
                  ? null
                  : (value) async {
                      await bind.mainSetUserDefaultOption(
                          key: kOptionCodecPreference, value: value);
                    },
            ),
          ],
        ),
        SettingsSection(
          title: Text(translate('Other Default Options')),
          tiles:
              otherDefaultSettings().map((e) => otherRow(e.$1, e.$2)).toList(),
        ),
      ]),
    );
  }

  SettingsTile otherRow(String label, String key) {
    final value = bind.mainGetUserDefaultOption(key: key) == 'Y';
    final isOptFixed = isOptionFixed(key);
    return SettingsTile.switchTile(
      initialValue: value,
      title: Text(translate(label)),
      onToggle: isOptFixed
          ? null
          : (b) async {
              await bind.mainSetUserDefaultOption(
                  key: key, value: b ? 'Y' : defaultOptionNo);
              setState(() {});
            },
    );
  }
}

class _ManageTrustedDevices extends StatefulWidget {
  const _ManageTrustedDevices();

  @override
  State<_ManageTrustedDevices> createState() => __ManageTrustedDevicesState();
}

class __ManageTrustedDevicesState extends State<_ManageTrustedDevices> {
  RxList<TrustedDevice> trustedDevices = RxList.empty(growable: true);
  RxList<Uint8List> selectedDevices = RxList.empty();

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(translate('Manage trusted devices')),
        centerTitle: true,
        actions: [
          Obx(() => IconButton(
              icon: Icon(Icons.delete, color: Colors.white),
              onPressed: selectedDevices.isEmpty
                  ? null
                  : () {
                      confrimDeleteTrustedDevicesDialog(
                          trustedDevices, selectedDevices);
                    }))
        ],
      ),
      body: FutureBuilder(
          future: TrustedDevice.get(),
          builder: (context, snapshot) {
            if (snapshot.connectionState == ConnectionState.waiting) {
              return Center(child: CircularProgressIndicator());
            }
            if (snapshot.hasError) {
              return Center(child: Text('Error: ${snapshot.error}'));
            }
            final devices = snapshot.data as List<TrustedDevice>;
            trustedDevices = devices.obs;
            return trustedDevicesTable(trustedDevices, selectedDevices);
          }),
    );
  }
}

class _RadioEntry {
  final String label;
  final String value;
  _RadioEntry(this.label, this.value);
}

typedef _RadioEntryGetter = String Function();
typedef _RadioEntrySetter = Future<void> Function(String);

SettingsTile _getPopupDialogRadioEntry({
  required String title,
  required List<_RadioEntry> list,
  required _RadioEntryGetter getter,
  required _RadioEntrySetter? asyncSetter,
  Widget? tail,
  RxBool? showTail,
  String? notCloseValue,
}) {
  RxString groupValue = ''.obs;
  RxString valueText = ''.obs;

  init() {
    groupValue.value = getter();
    final e = list.firstWhereOrNull((e) => e.value == groupValue.value);
    if (e != null) {
      valueText.value = e.label;
    }
  }

  init();

  void showDialog() async {
    gFFI.dialogManager.show((setState, close, context) {
      final onChanged = asyncSetter == null
          ? null
          : (String? value) async {
              if (value == null) return;
              await asyncSetter(value);
              init();
              if (value != notCloseValue) {
                close();
              }
            };

      return CustomAlertDialog(
          content: Obx(
        () => Column(children: [
          ...list
              .map((e) => getRadio(Text(translate(e.label)), e.value,
                  groupValue.value, onChanged))
              .toList(),
          Offstage(
            offstage:
                !(tail != null && showTail != null && showTail.value == true),
            child: tail,
          ),
        ]),
      ));
    }, backDismiss: true, clickMaskDismiss: true);
  }

  return SettingsTile(
    title: Text(translate(title)),
    onPressed: asyncSetter == null ? null : (context) => showDialog(),
    value: Padding(
      padding: EdgeInsets.symmetric(vertical: 8),
      child: Obx(() => Text(translate(valueText.value))),
    ),
  );
}

String displayNameLabel() {
  final name = bind.mainGetLocalOption(key: kOptionDisplayName);
  return name.isEmpty ? translate('Not set') : name;
}

void showDisplayName(VoidCallback refresh) {
  final controller = TextEditingController(
      text: bind.mainGetLocalOption(key: kOptionDisplayName));
  gFFI.dialogManager.show((setState, close, context) {
    submit() async {
      await bind.mainSetLocalOption(
          key: kOptionDisplayName, value: controller.text.trim());
      close();
      refresh();
    }

    return CustomAlertDialog(
      title: Text(translate('Display Name')),
      content: TextField(
        controller: controller,
        autofocus: true,
        maxLength: 64,
      ),
      actions: [
        dialogButton('Cancel', onPressed: close, isOutline: true),
        dialogButton('OK', onPressed: submit),
      ],
      onSubmit: submit,
      onCancel: close,
    );
  });
}

String dashboardStatusLabel(bool linking) {
  if (linking) return translate('Linking...');
  final uid = bind.mainGetOptionSync(key: "dashboard_user_id");
  if (uid.isEmpty) return translate('Not linked');
  final account = bind.mainGetOptionSync(key: "dashboard_account_name");
  return account.isNotEmpty ? account : translate('Linked');
}

void showDashboardLink(
    {required VoidCallback refresh, required void Function(bool) setLinking}) {
  final controller = TextEditingController();
  void pollLinked() {
    setLinking(true);
    var tries = 0;
    Timer.periodic(const Duration(seconds: 1), (t) {
      tries++;
      final linked = bind.mainGetOptionSync(key: "dashboard_user_id").isNotEmpty;
      if (linked || tries >= 20) {
        t.cancel();
        setLinking(false);
        refresh();
      }
    });
  }

  gFFI.dialogManager.show((setState, close, context) {
    final uid = bind.mainGetOptionSync(key: "dashboard_user_id");
    final linked = uid.isNotEmpty;
    final account = bind.mainGetOptionSync(key: "dashboard_account_name");
    final unlinkLocked =
        bind.mainGetOptionSync(key: "account-access-only") == "Y";
    return CustomAlertDialog(
      title: Text(translate(linked ? "Dashboard" : "Add to Dashboard")),
      content: linked
          ? Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(account.isEmpty
                    ? translate("This device is linked to your dashboard.")
                    : "${translate("Linked to")} $account"),
                const SizedBox(height: 8.0),
                Text("${translate("Dashboard ID")}: $uid",
                    style: Theme.of(context).textTheme.bodySmall),
                const SizedBox(height: 10.0),
                Text(
                    translate(
                        "Keeping this device linked enables continuous monitoring but may use additional battery. For always-on devices, keep it plugged in."),
                    style: Theme.of(context)
                        .textTheme
                        .bodySmall
                        ?.copyWith(color: Colors.orange)),
              ],
            )
          : Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(translate(
                    "Enter your dashboard invite code to manage this device from the dashboard.")),
                const SizedBox(height: 8.0),
                TextField(
                  enableInteractiveSelection: true,
                  decoration: InputDecoration(
                    hintText: translate("Invite code"),
                    suffixIcon: IconButton(
                      icon: const Icon(Icons.content_paste, size: 18),
                      tooltip: translate("Paste"),
                      onPressed: () async {
                        final data =
                            await Clipboard.getData(Clipboard.kTextPlain);
                        final text = data?.text?.trim() ?? '';
                        if (text.isNotEmpty) {
                          controller.text = text;
                          controller.selection = TextSelection.collapsed(
                              offset: controller.text.length);
                        }
                      },
                    ),
                  ),
                  controller: controller,
                  autofocus: true,
                ),
              ],
            ),
      actions: linked
          ? [
              dialogButton("Close", onPressed: close, isOutline: true),
              if (!unlinkLocked)
                dialogButton("Unlink", onPressed: () async {
                  await bind.mainSetOption(key: "dashboard_user_id", value: "");
                  await bind.mainSetOption(key: "dashboard_device_id", value: "");
                  await bind.mainSetOption(key: "dashboard_account_name", value: "");
                  await bind.mainSetOption(key: "invite_code", value: "");
                  if (isAndroid) gFFI.invokeMethod("stop_dashboard_service");
                  close();
                  refresh();
                }),
            ]
          : [
              dialogButton("Cancel", onPressed: close, isOutline: true),
              dialogButton("Link", onPressed: () async {
                final code = controller.text.trim();
                if (code.isEmpty) {
                  close();
                  return;
                }
                await bind.mainSetOption(key: "invite_code", value: code);
                close();
                if (isAndroid) {
                  gFFI.invokeMethod("start_dashboard_service");
                  AndroidPermissionManager.request(
                      kRequestIgnoreBatteryOptimizations);
                }
                pollLinked();
              }),
            ],
      onCancel: close,
    );
  });
}

Future<bool> _testTcpReachable(String host, int port, {int timeoutSec = 5}) async {
  try {
    final socket = await Socket.connect(host, port,
        timeout: Duration(seconds: timeoutSec));
    socket.destroy();
    return true;
  } catch (_) {
    return false;
  }
}

Future<String?> changeCustomApiUrl(String currentUrl) async {
  final controller = TextEditingController(text: currentUrl);
  final result =
      await gFFI.dialogManager.show<String>((setState, close, context) {
    var errorText = '';
    var isValidating = false;
    var validationStatus = '';
    return CustomAlertDialog(
      title: Text(translate("Custom Network URL")),
      content: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(translate("Enter your custom network settings URL:")),
          const SizedBox(height: 8.0),
          Row(
            children: [
              Expanded(
                child: TextField(
                    maxLines: null,
                    keyboardType: TextInputType.url,
                    decoration: InputDecoration(
                        hintText: 'https://',
                        isCollapsed: true,
                        suffix: IconButton(
                            padding: EdgeInsets.zero,
                            icon: const Icon(Icons.clear, size: 16),
                            onPressed: () => controller.clear()),
                        errorText: errorText.isEmpty ? null : errorText),
                    controller: controller,
                    autofocus: true),
              ),
            ],
          ),
          if (isValidating) ...[
            const SizedBox(height: 8.0),
            const LinearProgressIndicator(),
          ],
          if (validationStatus.isNotEmpty) ...[
            const SizedBox(height: 8.0),
            Text(validationStatus,
                style: Theme.of(context).textTheme.bodySmall),
          ],
        ],
      ),
      actions: [
        dialogButton("Cancel", onPressed: close, isOutline: true),
        dialogButton("OK",
            onPressed: isValidating
                ? null
                : () async {
                    final url = controller.text.trim();
                    if (url.isEmpty) {
                      close();
                      return;
                    }

                    final uri = Uri.tryParse(url);
                    if (uri == null ||
                        !uri.hasScheme ||
                        !uri.hasAuthority ||
                        (!uri.scheme.startsWith('http'))) {
                      setState(() {
                        errorText = translate("Invalid URL");
                      });
                      return;
                    }

                    setState(() {
                      isValidating = true;
                      errorText = '';
                      validationStatus = 'Fetching network settings...';
                    });

                    try {
                      final httpClient = HttpClient();
                      httpClient.connectionTimeout =
                          const Duration(seconds: 10);
                      final request = await httpClient.getUrl(uri);
                      final response = await request.close();
                      final body =
                          await response.transform(utf8.decoder).join();
                      httpClient.close();

                      final json = jsonDecode(body);
                      if (json is! Map ||
                          !json.containsKey('turnservers') ||
                          (!json.containsKey('rendezvous') &&
                              !json.containsKey('rendezvousssl'))) {
                        setState(() {
                          isValidating = false;
                          validationStatus = '';
                          errorText =
                              "The custom network URL did not return a correct response.";
                        });
                        return;
                      }

                      String? rendezvousHost;
                      int? rendezvousPort;
                      final rendezvousObj =
                          json['rendezvous'] ?? json['rendezvousssl'];
                      if (rendezvousObj is Map &&
                          rendezvousObj['host'] != null &&
                          rendezvousObj['port'] != null) {
                        rendezvousHost = rendezvousObj['host'].toString();
                        rendezvousPort =
                            int.tryParse(rendezvousObj['port'].toString());
                      }

                      int turnCount = 0;
                      final turnServers = json['turnservers'];
                      if (turnServers is List) {
                        turnCount = turnServers.length;
                      }

                      if (rendezvousHost == null || rendezvousPort == null) {
                        setState(() {
                          isValidating = false;
                          validationStatus = '';
                          errorText =
                              "Rendezvous server not found in response. Expected 'rendezvous' or 'rendezvousssl' with 'host' and 'port'.";
                        });
                        return;
                      }

                      setState(() {
                        validationStatus =
                            'Testing rendezvous server $rendezvousHost:$rendezvousPort...';
                      });

                      final rendezvousOk = await _testTcpReachable(
                          rendezvousHost, rendezvousPort);

                      bool? turnOk;
                      String turnAddr = '';
                      if (turnServers is List && turnServers.isNotEmpty) {
                        final firstTurn = turnServers[0];
                        if (firstTurn is Map &&
                            firstTurn['host'] != null &&
                            firstTurn['port'] != null) {
                          final turnHost = firstTurn['host'].toString();
                          final turnPort =
                              int.tryParse(firstTurn['port'].toString());
                          if (turnPort != null) {
                            turnAddr = '$turnHost:$turnPort';
                            setState(() {
                              validationStatus =
                                  'Testing TURN server $turnAddr...';
                            });
                            turnOk =
                                await _testTcpReachable(turnHost, turnPort);
                          }
                        }
                      }

                      if (!rendezvousOk) {
                        setState(() {
                          isValidating = false;
                          validationStatus = '';
                          errorText =
                              "Rendezvous server $rendezvousHost:$rendezvousPort is not reachable. Please check the server is running and accessible.";
                        });
                        return;
                      }

                      var statusParts = <String>[];
                      statusParts.add(
                          'Rendezvous: $rendezvousHost:$rendezvousPort OK');
                      if (turnOk == true) {
                        statusParts.add('TURN: $turnAddr OK ($turnCount server${turnCount != 1 ? 's' : ''})');
                      } else if (turnOk == false) {
                        statusParts.add(
                            'Warning: TURN server $turnAddr not reachable. Relay connections may not work.');
                      } else if (turnCount == 0) {
                        statusParts.add(
                            'Warning: No TURN servers configured. Relay connections will not work.');
                      }

                      await bind.mainSetOption(
                          key: kOptionCustomApiUrl, value: url);
                      close(url);
                    } catch (e) {
                      setState(() {
                        isValidating = false;
                        validationStatus = '';
                        errorText =
                            "The custom network URL provided was not valid, please try again.";
                      });
                    }
                  }),
      ],
      onCancel: close,
    );
  });
  return result;
}
