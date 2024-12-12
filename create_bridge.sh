#!/bin/sh

# 函数：检查 TAP 设备是否存在
check_tap_exists() {
    if ip link show "$1" > /dev/null 2>&1; then
        echo "TAP device $1 already exists"
        return 0
    else
        return 1
    fi
}

# 函数：检查是否已经为设备分配了 IP 地址
check_ip_assigned() {
    if ip addr show "$1" | grep -q "$2"; then
        echo "IP address $2 already assigned to $1"
        return 0
    else
        return 1
    fi
}

# 创建 TAP 设备，如果不存在
create_tap_device() {
    if check_tap_exists "$1"; then
        return 0
    else
        if sudo ip tuntap add dev "$1" mode tap; then
            echo "TAP device $1 created"
        else
            echo "Failed to create TAP device $1" >&2
            exit 1
        fi
    fi
}

# 启用 TAP 设备
enable_tap_device() {
    if sudo ip link set "$1" up; then
        echo "TAP device $1 enabled"
    else
        echo "Failed to enable TAP device $1" >&2
        exit 1
    fi
}

# 为 TAP 设备分配 IP 地址，如果还未分配
assign_ip_to_tap() {
    if check_ip_assigned "$1" "$2"; then
        return 0
    else
        if sudo ip addr add "$2" dev "$1"; then
            echo "IP address $2 assigned to $1"
        else
            echo "Failed to assign IP address $2 to $1" >&2
            exit 1
        fi
    fi
}

# 创建并配置 TAP 设备
create_tap_device "tap0"
create_tap_device "tap1"
create_tap_device "tap2"

printf "TAP devices created or confirmed to exist\n"

# 启用 TAP 设备
enable_tap_device "tap0"
enable_tap_device "tap1"
enable_tap_device "tap2"

printf "TAP devices enabled\n"

# 为 TAP 设备分配 IP 地址
assign_ip_to_tap "tap0" "192.168.69.2/24"
assign_ip_to_tap "tap1" "192.168.69.5/24"
assign_ip_to_tap "tap2" "192.168.69.6/24"

printf "IP addresses assigned to TAP devices\n"

# 禁用网桥的防火墙
if echo 0 | sudo tee /proc/sys/net/bridge/bridge-nf-call-iptables; then
    echo "Bridge iptables disabled"
else
    echo "Failed to disable bridge iptables" >&2
    exit 1
fi

if echo 0 | sudo tee /proc/sys/net/bridge/bridge-nf-call-ip6tables; then
    echo "Bridge ip6tables disabled"
else
    echo "Failed to disable bridge ip6tables" >&2
    exit 1
fi

if echo 0 | sudo tee /proc/sys/net/bridge/bridge-nf-call-arptables; then
    echo "Bridge arptables disabled"
else
    echo "Failed to disable bridge arptables" >&2
    exit 1
fi

# 创建一个网桥
if ip link show br0 > /dev/null 2>&1; then
    echo "Bridge br0 already exists"
else
    if sudo ip link add name br0 type bridge; then
        echo "Bridge br0 created"
    else
        echo "Failed to create bridge br0" >&2
        exit 1
    fi
fi

# 将 TAP 设备添加到网桥
if sudo ip link set tap1 master br0; then
    echo "tap1 added to bridge br0"
else
    echo "Failed to add tap1 to bridge br0" >&2
    exit 1
fi

if sudo ip link set tap2 master br0; then
    echo "tap2 added to bridge br0"
else
    echo "Failed to add tap2 to bridge br0" >&2
    exit 1
fi

printf "Bridge created and TAP devices added to it\n"

# 启用网桥
if sudo ip link set br0 up; then
    echo "Bridge br0 enabled"
else
    echo "Failed to enable bridge br0" >&2
    exit 1
fi

printf "Bridge enabled\n"

printf "Bridge firewall rules disabled\n"

# 验证配置
ip addr show
bridge link show


# sudo ip link set tap0 down
# sudo ip tuntap del dev tap0 mode tap
# sudo brctl delbr <bridge_name>
# sudo ip link delete br0 type bridge 