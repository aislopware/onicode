// @peculiar/x509 resolves its crypto provider through tsyringe, which needs this polyfill first.
import "reflect-metadata";

export { X509Certificate } from "@peculiar/x509";
