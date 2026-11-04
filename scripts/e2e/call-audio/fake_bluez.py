#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""A fake BlueZ (org.bluez) with one adapter and one phone, for the call-audio sandbox.

Runs on a *private* system bus (DBUS_SYSTEM_BUS_ADDRESS) next to a private PipeWire and
WirePlumber. PipeWire's bluez5 plugin sees adapter hci0 and a paired, connected phone that offers
the hands-free audio gateway (HFP AG). When PipeWire registers its hands-free (HF) profile, the
fake hands it an RFCOMM stand-in (one end of a socketpair) through Profile1.NewConnection and plays
the phone's side of HFP over it: the service-level connection (AT+BRSF, AT+CIND, AT+CMER, ...),
then incoming calls, answer (ATA), hang-up (AT+CHUP) and dialing (ATD) on request.

So PipeWire creates the real `bluez_card.<ADDR>` (profile `audio-gateway`) and the real
`org.pipewire.Telephony` audio gateway and call objects. What cannot be faked without a kernel
controller is the SCO voice link itself; the sandbox stands in for it with two pw-cat streams that
carry the same properties PipeWire gives the real ones (see call-audio-sandbox.sh).

Control (same private system bus), object /geekbridge/FakePhone, interface org.geekbridge.FakePhone:
  Ring(s number)      an incoming call (+CIEV callsetup=1, RING, +CLIP)
  RemoteAnswer()      the call is answered on the phone itself
  RemoteHangup()      the far end hangs up
  AtLog() -> as       every AT command PipeWire sent, in order
  State() -> s        idle | ringing | active | dialing
  Disconnect()        the phone goes out of range (RFCOMM closed, Connected=false)

Nothing here touches the real system bus: it refuses to start if DBUS_SYSTEM_BUS_ADDRESS is unset or
points at the real one.
"""

import os
import socket
import sys

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib

ADDR = os.environ.get("FAKE_PHONE_ADDR", "AA:BB:CC:00:11:22")
NAME = os.environ.get("FAKE_PHONE_NAME", "Sandbox Phone")
ADAPTER = "/org/bluez/hci0"
DEVICE = ADAPTER + "/dev_" + ADDR.replace(":", "_")
# A second phone, not paired yet: it shows up when discovery starts (the "Set up call audio" flow).
SETUP_ADDR = "AA:BB:CC:00:33:44"
SETUP_NAME = "Setup Phone"
SETUP_DEVICE = ADAPTER + "/dev_" + SETUP_ADDR.replace(":", "_")
PASSKEY = 482910
HFP_AG = "0000111f-0000-1000-8000-00805f9b34fb"
HFP_HF = "0000111e-0000-1000-8000-00805f9b34fb"
A2DP_SRC = "0000110a-0000-1000-8000-00805f9b34fb"
PROPS = "org.freedesktop.DBus.Properties"
OM = "org.freedesktop.DBus.ObjectManager"

# +CIND indicator order: service, call, callsetup, callheld, signal, roam, battchg
CIND_TEST = '+CIND: ("service",(0,1)),("call",(0,1)),("callsetup",(0-3)),("callheld",(0-2)),("signal",(0-5)),("roam",(0,1)),("battchg",(0-5))'
AG_FEATURES = 0x0001 | 0x0004 | 0x0008 | 0x0020 | 0x0040 | 0x0100 | 0x0200  # 3way, EC/NR, voice rec, reject, enh status, ext errors, codec neg


def log(*a):
    print("fake_bluez:", *a, file=sys.stderr, flush=True)


class Obj(dbus.service.Object):
    def __init__(self, bus, path, ifaces, fake=None):
        super().__init__(bus, path)
        self.path = path
        self.ifaces = ifaces  # name -> dict of props
        self.fake = fake

    @dbus.service.method(PROPS, in_signature="ss", out_signature="v")
    def Get(self, iface, name):
        return self.ifaces[iface][name]

    @dbus.service.method(PROPS, in_signature="s", out_signature="a{sv}")
    def GetAll(self, iface):
        return dbus.Dictionary(self.ifaces.get(iface, {}), signature="sv")

    @dbus.service.method(PROPS, in_signature="ssv")
    def Set(self, iface, name, value):
        self.ifaces[iface][name] = value
        self.PropertiesChanged(iface, {name: value}, [])

    @dbus.service.signal(PROPS, signature="sa{sv}as")
    def PropertiesChanged(self, iface, changed, invalidated):
        pass

    def set(self, iface, name, value):
        self.ifaces[iface][name] = value
        self.PropertiesChanged(iface, {name: value}, [])

    # Media1 (adapter) and Device1 (phone) methods. dbus-python registers methods per class, so
    # both objects answer both; PipeWire only calls the right one on each.
    @dbus.service.method("org.bluez.Media1", in_signature="oa{sv}")
    def RegisterApplication(self, path, opts):
        log("RegisterApplication", path)

    @dbus.service.method("org.bluez.Media1", in_signature="o")
    def UnregisterApplication(self, path):
        pass

    @dbus.service.method("org.bluez.Media1", in_signature="oa{sv}")
    def RegisterEndpoint(self, path, props):
        pass

    @dbus.service.method("org.bluez.Media1", in_signature="o")
    def UnregisterEndpoint(self, path):
        pass

    @dbus.service.method("org.bluez.Adapter1")
    def StartDiscovery(self):
        log("StartDiscovery")
        self.set("org.bluez.Adapter1", "Discovering", True)
        self.fake.discovered()

    @dbus.service.method("org.bluez.Adapter1")
    def StopDiscovery(self):
        self.set("org.bluez.Adapter1", "Discovering", False)

    @dbus.service.method("org.bluez.Device1", async_callbacks=("ok", "err"))
    def Pair(self, ok, err):
        log("Pair", self.path)
        self.fake.pair(self, ok, err)

    @dbus.service.method("org.bluez.Device1")
    def Connect(self):
        log("Connect", self.path)
        self.fake.device_connect(self)

    @dbus.service.method("org.bluez.Device1")
    def Disconnect(self):
        pass

    @dbus.service.method("org.bluez.Device1", in_signature="s")
    def ConnectProfile(self, uuid):
        log("ConnectProfile", self.path, uuid)
        if uuid.lower() == HFP_AG:
            self.fake.device_connect(self)

    @dbus.service.method("org.bluez.Device1", in_signature="s")
    def DisconnectProfile(self, uuid):
        pass


class Root(dbus.service.Object):
    def __init__(self, bus, objs):
        super().__init__(bus, "/")
        self.objs = objs

    @dbus.service.method(OM, out_signature="a{oa{sa{sv}}}")
    def GetManagedObjects(self):
        # /org/bluez first: PipeWire starts its hands-free backend when it sees ProfileManager1.
        out = {
            dbus.ObjectPath("/org/bluez"): {
                "org.bluez.ProfileManager1": dbus.Dictionary({}, signature="sv"),
                "org.bluez.AgentManager1": dbus.Dictionary({}, signature="sv"),
            }
        }
        for o in self.objs:
            out[dbus.ObjectPath(o.__dbus_object_path__)] = {
                k: dbus.Dictionary(v, signature="sv") for k, v in o.ifaces.items()
            }
        return out

    @dbus.service.signal(OM, signature="oa{sa{sv}}")
    def InterfacesAdded(self, path, ifaces):
        pass

    @dbus.service.signal(OM, signature="oas")
    def InterfacesRemoved(self, path, ifaces):
        pass


class Phone:
    """The phone's side of HFP (audio gateway) over the RFCOMM stand-in."""

    def __init__(self):
        self.sock = None
        self.watch = None
        self.buf = b""
        self.at_log = []
        self.state = "idle"
        self.number = ""
        self.cmer = False
        self.codec_selected = False

    def attach(self, sock):
        # A new RFCOMM link starts a new service-level connection.
        self.close()
        sock.setblocking(False)
        self.sock = sock
        self.buf = b""
        self.cmer = False
        self.codec_selected = False
        self.watch = GLib.io_add_watch(sock.fileno(), GLib.IO_IN | GLib.IO_HUP | GLib.IO_ERR, self.on_data)

    def close(self):
        """Drop the link; the watch goes first so a reused fd number is never read twice."""
        if getattr(self, "watch", None):
            GLib.source_remove(self.watch)
            self.watch = None
        if self.sock is not None:
            self.sock.close()
            self.sock = None

    def send(self, line):
        if self.sock is None:
            return
        log("AG ->", line)
        try:
            self.sock.send(("\r\n" + line + "\r\n").encode())
        except OSError as e:
            log("send failed", e)

    def ciev(self, ind, val):
        if self.cmer:
            self.send(f"+CIEV: {ind},{val}")

    def indicators(self):
        call = 1 if self.state == "active" else 0
        setup = {"ringing": 1, "dialing": 2}.get(self.state, 0)
        return f"+CIND: 1,{call},{setup},0,5,0,5"

    def on_data(self, fd, cond):
        if cond & (GLib.IO_HUP | GLib.IO_ERR) or self.sock is None:
            log("rfcomm closed by PipeWire")
            self.watch = None
            return False
        try:
            data = self.sock.recv(4096)
        except BlockingIOError:
            return True
        except OSError:
            self.watch = None
            return False
        if not data:
            log("rfcomm EOF")
            self.watch = None
            return False
        self.buf += data
        while b"\r" in self.buf:
            line, self.buf = self.buf.split(b"\r", 1)
            line = line.strip().decode(errors="replace")
            if line:
                self.at(line)
        return True

    def at(self, cmd):
        log("HF ->", cmd)
        self.at_log.append(cmd)
        c = cmd.upper()
        if c.startswith("AT+BRSF="):
            self.send(f"+BRSF: {AG_FEATURES}")
        elif c == "AT+CIND=?":
            self.send(CIND_TEST)
        elif c == "AT+CIND?":
            self.send(self.indicators())
        elif c.startswith("AT+CMER="):
            self.cmer = True
        elif c == "AT+CHLD=?":
            self.send("+CHLD: (0,1,2,3)")
        elif c == "AT+COPS?":
            self.send('+COPS: 0,0,"Sandbox"')
        elif c == "AT+CLCC":
            if self.state in ("ringing", "active", "dialing"):
                stat = {"ringing": 4, "active": 0, "dialing": 2}[self.state]
                d = 1 if self.state == "ringing" or self.number.startswith("+1555") else 0
                self.send(f'+CLCC: 1,{d},{stat},0,0,"{self.number}",145')
        elif c == "AT+CNUM":
            pass
        elif c == "ATA":
            if self.state == "ringing":
                self.send("OK")
                self.state = "active"
                self.ciev(2, 1)
                self.ciev(3, 0)
                return
            self.send("ERROR")
            return
        elif c == "AT+CHUP":
            if self.state in ("ringing", "active", "dialing"):
                was = self.state
                self.send("OK")
                self.state = "idle"
                if was == "active":
                    self.ciev(2, 0)
                else:
                    self.ciev(3, 0)
                return
            self.send("ERROR")
            return
        elif c.startswith("ATD"):
            self.number = cmd[3:].rstrip(";")
            self.send("OK")
            self.state = "dialing"
            self.ciev(3, 2)
            # The far end picks up after a second.
            GLib.timeout_add(1000, self.far_end_answers)
            return
        elif c.startswith("AT+BCS="):
            pass
        elif c.startswith("AT+VGM=") and not self.codec_selected:
            # End of PipeWire's setup sequence: select a codec like Android does (2 = mSBC,
            # 1 = CVSD via FAKE_PHONE_CODEC). PipeWire creates the transport and the card then.
            self.send("OK")
            self.codec_selected = True
            self.send("+BCS: " + os.environ.get("FAKE_PHONE_CODEC", "2"))
            return
        elif c == "AT+BCC":
            # The HF asks for the voice link; no SCO can be made here, but the request is logged.
            pass
        self.send("OK")

    def far_end_answers(self):
        if self.state == "dialing":
            self.state = "active"
            self.ciev(3, 3)
            self.ciev(2, 1)
            self.ciev(3, 0)
        return False

    def ring(self, number):
        self.number = number
        self.state = "ringing"
        self.ciev(3, 1)
        self.send("RING")
        self.send(f'+CLIP: "{number}",145')

    def remote_answer(self):
        if self.state == "ringing":
            self.state = "active"
            self.ciev(2, 1)
            self.ciev(3, 0)

    def remote_hangup(self):
        if self.state == "active":
            self.ciev(2, 0)
        elif self.state in ("ringing", "dialing"):
            self.ciev(3, 0)
        self.state = "idle"


class Control(dbus.service.Object):
    def __init__(self, bus, phone, fake):
        super().__init__(bus, "/geekbridge/FakePhone")
        self.phone = phone
        self.fake = fake

    @dbus.service.method("org.geekbridge.FakePhone", in_signature="s")
    def Ring(self, number):
        self.phone.ring(number)

    @dbus.service.method("org.geekbridge.FakePhone")
    def RemoteAnswer(self):
        self.phone.remote_answer()

    @dbus.service.method("org.geekbridge.FakePhone")
    def RemoteHangup(self):
        self.phone.remote_hangup()

    @dbus.service.method("org.geekbridge.FakePhone", out_signature="as")
    def AtLog(self):
        return dbus.Array(self.phone.at_log, signature="s")

    @dbus.service.method("org.geekbridge.FakePhone", out_signature="s")
    def State(self):
        return self.phone.state

    @dbus.service.method("org.geekbridge.FakePhone", out_signature="b")
    def Connected(self):
        return self.phone.sock is not None and len(self.phone.at_log) > 0

    @dbus.service.method("org.geekbridge.FakePhone")
    def Disconnect(self):
        self.fake.disconnect()

    @dbus.service.method("org.geekbridge.FakePhone")
    def Forget(self):
        """Unpair the setup phone again (so a setup can pair it from scratch)."""
        self.fake.forget()


class Fake:
    def __init__(self, bus):
        self.bus = bus
        self.phone = Phone()
        self.phones = {DEVICE: self.phone}
        self.profiles = {}  # uuid -> (sender, path)
        self.agent = None  # (sender, path)
        self.setup_device = None
        self.adapter = Obj(bus, ADAPTER, {
            "org.bluez.Adapter1": {
                "Address": "00:1A:7D:DA:71:13",
                "AddressType": "public",
                "Name": "sandbox",
                "Alias": "sandbox",
                "Class": dbus.UInt32(0x6c010c),
                "Powered": True,
                "PowerState": "on",
                "Discoverable": False,
                "Pairable": True,
                "Discovering": False,
                "UUIDs": dbus.Array([HFP_HF], signature="s"),
                "Modalias": "usb:v1D6Bp0246d0550",
                "Roles": dbus.Array(["central", "peripheral"], signature="s"),
            },
            "org.bluez.Media1": {
                "SupportedUUIDs": dbus.Array([], signature="s"),
            },
        }, self)
        self.device = Obj(bus, DEVICE, {
            "org.bluez.Device1": {
                "Address": ADDR,
                "AddressType": "public",
                "Name": NAME,
                "Alias": NAME,
                "Class": dbus.UInt32(0x5a020c),
                "Icon": "phone",
                "Paired": True,
                "Bonded": True,
                "Trusted": True,
                "Blocked": False,
                "LegacyPairing": False,
                "Connected": True,
                "ServicesResolved": True,
                "UUIDs": dbus.Array([HFP_AG, A2DP_SRC], signature="s"),
                "Adapter": dbus.ObjectPath(ADAPTER),
                "Modalias": "bluetooth:v0075p0100d0201",
            },
        }, self)
        self.bluez = BluezRoot(bus, self)
        self.root = Root(bus, [self.adapter, self.device])
        self.control = Control(bus, self.phone, self)

    def register_profile(self, sender, path, uuid, opts):
        log("RegisterProfile", sender, path, uuid)
        self.profiles[uuid] = (sender, path)
        if uuid == HFP_HF:
            GLib.timeout_add(300, self.connect_hfp)

    def discovered(self):
        if self.setup_device is not None:
            return
        self.setup_device = Obj(self.bus, SETUP_DEVICE, {
            "org.bluez.Device1": {
                "Address": SETUP_ADDR,
                "AddressType": "public",
                "Name": SETUP_NAME,
                "Alias": SETUP_NAME,
                "Class": dbus.UInt32(0x5a020c),
                "Icon": "phone",
                "Paired": False,
                "Bonded": False,
                "Trusted": False,
                "Blocked": False,
                "LegacyPairing": False,
                "Connected": False,
                "ServicesResolved": False,
                "RSSI": dbus.Int16(-50),
                "UUIDs": dbus.Array([], signature="s"),
                "Adapter": dbus.ObjectPath(ADAPTER),
            },
        }, self)
        self.root.objs.append(self.setup_device)
        self.root.InterfacesAdded(dbus.ObjectPath(SETUP_DEVICE), {
            k: dbus.Dictionary(v, signature="sv") for k, v in self.setup_device.ifaces.items()
        })

    def pair(self, dev, ok, err):
        if self.agent is None:
            err(dbus.exceptions.DBusException("no agent", name="org.bluez.Error.AuthenticationFailed"))
            return
        sender, path = self.agent
        agent = self.bus.get_object(sender, path)

        def confirmed():
            log("agent confirmed the passkey")
            d = dev.ifaces["org.bluez.Device1"]
            dev.set("org.bluez.Device1", "Paired", True)
            dev.set("org.bluez.Device1", "Bonded", True)
            # Services are known after pairing.
            dev.set("org.bluez.Device1", "UUIDs", dbus.Array([HFP_AG, A2DP_SRC], signature="s"))
            d["ServicesResolved"] = True
            ok()

        def rejected(e):
            log("agent rejected:", e)
            err(dbus.exceptions.DBusException("rejected", name="org.bluez.Error.AuthenticationRejected"))

        agent.RequestConfirmation(dbus.ObjectPath(dev.path), dbus.UInt32(PASSKEY),
                                  dbus_interface="org.bluez.Agent1", timeout=120,
                                  reply_handler=confirmed, error_handler=rejected)

    def device_connect(self, dev):
        d = dev.ifaces["org.bluez.Device1"]
        if not d.get("Paired"):
            raise dbus.exceptions.DBusException("not paired", name="org.bluez.Error.Failed")
        if dev.path not in self.phones:
            self.phones[dev.path] = Phone()
        if not d.get("Connected"):
            dev.set("org.bluez.Device1", "Connected", True)
        GLib.timeout_add(100, lambda: self.connect_hfp(dev.path))

    def connect_hfp(self, device=DEVICE):
        phone = self.phones[device]
        if HFP_HF not in self.profiles or phone.sock is not None:
            return False
        sender, path = self.profiles[HFP_HF]
        mine, theirs = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
        phone.attach(mine)
        prof = self.bus.get_object(sender, path)
        opts = dbus.Dictionary({"Version": dbus.UInt16(0x0108), "Features": dbus.UInt16(0x2f)}, signature="sv")
        log("NewConnection ->", sender, path, device)
        prof.NewConnection(dbus.ObjectPath(device), dbus.types.UnixFd(theirs), opts,
                           dbus_interface="org.bluez.Profile1",
                           reply_handler=lambda: log("NewConnection accepted"),
                           error_handler=lambda e: log("NewConnection failed:", e))
        theirs.close()
        return False

    def forget(self):
        dev = self.setup_device
        if dev is None:
            return
        phone = self.phones.pop(dev.path, None)
        if phone is not None:
            phone.close()
        for k, v in (("Paired", False), ("Bonded", False), ("Trusted", False), ("Connected", False)):
            dev.set("org.bluez.Device1", k, v)
        dev.set("org.bluez.Device1", "UUIDs", dbus.Array([], signature="s"))

    def disconnect(self):
        self.phone.close()
        self.device.set("org.bluez.Device1", "Connected", False)


class BluezRoot(dbus.service.Object):
    """/org/bluez: ProfileManager1 (and an AgentManager1 that accepts anything)."""

    def __init__(self, bus, fake):
        super().__init__(bus, "/org/bluez")
        self.fake = fake

    @dbus.service.method("org.bluez.ProfileManager1", in_signature="osa{sv}", sender_keyword="sender")
    def RegisterProfile(self, path, uuid, opts, sender=None):
        self.fake.register_profile(sender, path, uuid, opts)

    @dbus.service.method("org.bluez.ProfileManager1", in_signature="o")
    def UnregisterProfile(self, path):
        pass

    @dbus.service.method("org.bluez.AgentManager1", in_signature="os", sender_keyword="sender")
    def RegisterAgent(self, path, cap, sender=None):
        log("RegisterAgent", sender, path, cap)
        self.fake.agent = (sender, path)

    @dbus.service.method("org.bluez.AgentManager1", in_signature="o")
    def RequestDefaultAgent(self, path):
        log("RequestDefaultAgent", path)

    @dbus.service.method("org.bluez.AgentManager1", in_signature="o")
    def UnregisterAgent(self, path):
        log("UnregisterAgent", path)
        self.fake.agent = None


def main():
    addr = os.environ.get("DBUS_SYSTEM_BUS_ADDRESS", "")
    if not addr or "/run/dbus/system_bus_socket" in addr:
        log("refusing: DBUS_SYSTEM_BUS_ADDRESS must point at a private bus")
        return 2
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.bus.BusConnection(addr)
    fake = Fake(bus)
    # Keep the name object alive: dbus-python releases the name when it is collected.
    name = dbus.service.BusName("org.bluez", bus)
    log("ready on", addr)
    GLib.MainLoop().run()
    del fake, name
    return 0


if __name__ == "__main__":
    sys.exit(main())
