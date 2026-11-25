-- host.luau: runs on the host (the machine running glassdeckd) and sends data to its keyboard half's on_data.

declare function every(seconds: number, callback: () -> ()): ()

-- The latest value wins: a send replaces anything of this widget's not yet on the Link.
declare function send(data: Data): ()

declare system: {
    cpu_percent: () -> number,
    memory_percent: () -> number,
}

-- assets/host/<name>: stays on the host, so only host.luau has this table
declare host: { resource: (name: string) -> Resource }
