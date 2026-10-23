//! Bitmap and SNS inscription filters, identical to the ones `db_reader`
//! applies in `getBlockBitmapInscrs` and `getBlockSNSInscrs`.

use db_reader::InscriptionInfo;

pub fn is_valid_bitmap(inscription_info: &InscriptionInfo) -> bool {
    if inscription_info.inscription_number < 0 {
        return false;
    }
    if inscription_info.is_json {
        return false;
    }
    if !inscription_info
        .content_type_hex
        .to_lowercase()
        .starts_with("746578742f706c61696e")
    {
        return false;
    }

    true
}

pub fn is_valid_sns(inscription_info: &InscriptionInfo) -> bool {
    if inscription_info.inscription_number < 0 {
        return false;
    }
    if !inscription_info
        .content_type_hex
        .to_lowercase()
        .starts_with("746578742f706c61696e")
        && !inscription_info
            .content_type_hex
            .to_lowercase()
            .starts_with("6170706c69636174696f6e2f6a736f6e")
    {
        return false;
    }

    true
}
