import { invoke } from "@tauri-apps/api/core";

function call(command, payload) {
  return invoke(command, payload);
}

window.oneclaw = {
  detectInstallation: () => call("detect_installation"),
  resolveConflict: (params) => call("resolve_conflict", { params }),
  setupGetLaunchAtLogin: () => call("setup_get_launch_at_login"),
  verifyKey: (params) => call("verify_key", { params }),
  saveConfig: (params) => call("save_config", { params }),
  completeSetup: (params) => call("complete_setup", { params }),
  getGatewayStatus: () => call("gateway_status"),
  restartGateway: () => call("restart_gateway"),
  testOpenClawChat: () => call("test_openclaw_chat"),
  kimiOAuthLogin: () => call("kimi_oauth_login"),
  kimiOAuthCancel: () => call("kimi_oauth_cancel"),
  kimiOAuthLogout: () => call("kimi_oauth_logout"),
  kimiOAuthStatus: () => call("kimi_oauth_status"),
  openExternal: (url) => call("open_external", { url }),
};
