# Copyright (c) 2021 The Bitcoin Core developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or http://www.opensource.org/licenses/mit-license.php.

@0xf2c5cfa319406aa6;

using Cxx = import "/capnp/c++.capnp";
$Cxx.namespace("ipc::capnp::messages");

using Proxy = import "proxy.capnp";
using Chain = import "chain.capnp";
using Echo = import "echo.capnp";
using Mining = import "mining.capnp";

interface Init $Proxy.wrap("interfaces::Init") {
    construct @0 (threadMap: Proxy.ThreadMap) -> (threadMap :Proxy.ThreadMap);
    makeEcho @1 (context :Proxy.Context) -> (result :Echo.Echo);

    # DEPRECATED: no longer supported; server returns an error.
    makeMiningOld2 @2 () -> ();

    makeMining @3 (context :Proxy.Context) -> (result :Mining.Mining);

    # Upstream uses ordinal @4 for `makeRpc`. This Rust crate does not bind
    # the Rpc interface yet, but the ordinal must be present so the schema
    # matches the wire protocol. Declared as a no-op stub.
    makeRpcStub @4 () -> ();

    makeChain @5 (context :Proxy.Context) -> (result :Chain.Chain);
}
