// Browser / Workers entry (lite build: conversion only, smallest download).
import init, * as raw from "../dist/lite-web/slidepdf_wasm.js";
import { makeWebApi } from "../web-api.js";

const api = makeWebApi(init, raw, new URL("../dist/lite-web/slidepdf_wasm_bg.wasm", import.meta.url));
export const { ready, convert, convertStream, assemble, assembleStream, split, fragmentHash, verifyFragment, search } = api;
