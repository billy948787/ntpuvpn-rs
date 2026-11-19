// OpenConnect invokes Windows vpnc scripts through cscript.exe. Only pass
// numeric tunnel settings to Rust; never run server-provided shell commands.
var shell = new ActiveXObject("WScript.Shell");
var env = shell.Environment("PROCESS");
if (env("reason") == "connect" || env("reason") == "reconnect") {
    var fields = ["TUNIDX", "INTERNAL_IP4_ADDRESS", "INTERNAL_IP4_NETMASK", "INTERNAL_IP4_MTU"];
    var values = [];
    for (var i = 0; i < fields.length; i++) {
        var value = env(fields[i]);
        if (!/^[0-9.]+$/.test(value)) WScript.Quit(1);
        values.push(value);
    }
    // A new event also restores routes when the assigned address is unchanged.
    values.push(String(new Date().getTime()));
    var fs = new ActiveXObject("Scripting.FileSystemObject");
    var path = env("NTPUVPN_TUNNEL_STATE");
    var file = fs.CreateTextFile(path + ".tmp", true, false);
    file.Write(values.join("\n"));
    file.Close();
    if (fs.FileExists(path)) fs.DeleteFile(path);
    fs.MoveFile(path + ".tmp", path);
}
