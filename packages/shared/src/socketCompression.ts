// A client sets this to "off" when its socket never leaves the machine, where deflate
// only costs CPU. Old clients omit it; old servers ignore it.
export const SOCKET_COMPRESSION_QUERY_PARAM = "compression";
