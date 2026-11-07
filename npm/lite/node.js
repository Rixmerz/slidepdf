// Node.js entry (lite build: conversion only, no font embedding, no assemble/split).
import { makeNodeApi, loadRaw } from "../node-api.js";

const api = makeNodeApi(loadRaw("./dist/lite-node/slidepdf_wasm.js"));
export const { convert, convertStream, convertFile, assemble, assembleStream, assembleFiles, split, fragmentHash, verifyFragment, search, ready } = api;
