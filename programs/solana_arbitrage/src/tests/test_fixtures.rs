use crate::InstructionData;

pub const PUBKEYS_LIST: &[(&str, bool)] = &[
    ("FYnaLRpfVbAi5CnupX1JuxqokiR773WiZPiCz3dzp7BP", true),  // payer [0] w
    ("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", false),  // token_program [1] r
    ("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb", false),  // token_program_2022 [2] r
    ("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr", true),  // memo [3] w
    ("So11111111111111111111111111111111111111112", false),  // wsol_mint [4] r
    ("Ft6ingqkyR9JkdddhFUhTtKozr2ZbZssA9nu7sPLNtsk", true),  // user_wsol_ata [5] w
    ("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", false),  // mint [6] r
    ("NRhkUWesrRaxHXgFCHcFwi7uLJHeAadxt8DZWKBm6aL", true),  // user_token_ata [7] w
    ("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", false),  // meteora_dlmm program_id [8] r
    ("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", false),  // meteora_dlmm host_fee_in [9] r
    ("D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6", false),  // meteora_dlmm event_authority [10] r
    ("1jw5fDodwGEGBVqNXsx2eqiLgNmgMDEeXWSbrTreLCM", true),  // meteora_dlmm [1jw5fDod] pool_id [11] w
    ("GLFpCS3jPrQ2y2yCyQWb4Uiz3aEeDSQVqQcaM4rhGxUa", true),  // meteora_dlmm [1jw5fDod] base_vault [12] w
    ("DyDE7RLGZStDSMxBVp4RMRnNWmN5LNLTwXWajCKGpURx", true),  // meteora_dlmm [1jw5fDod] quote_vault [13] w
    ("EAE5ZuXW2vyjur1V7KdFhE9XXRcGDHGVNQTVjR2vfkTP", true),  // meteora_dlmm [1jw5fDod] oracle [14] w
    ("2bV7mtxnqK1AzQjyDs9AbeTaQaJgjw9VFFwrQGdbQchV", true),  // meteora_dlmm [1jw5fDod] bitmap_ext [15] w
    ("AVdh2KuqFQpxzZothzEyCnY58T8w5fjPxw321ogez2eX", true),  // meteora_dlmm [1jw5fDod] bin_array_buy_0 [16] w
    ("AVdh2KuqFQpxzZothzEyCnY58T8w5fjPxw321ogez2eX", true),  // meteora_dlmm [1jw5fDod] bin_array_buy_1 [17] w
    ("5rCf1DM8LjKTw4YqhnoLcngyZYeNnQqztScTogYHAS6", true),  // meteora_dlmm [5rCf1DM8] pool_id [18] w
    ("EYj9xKw6ZszwpyNibHY7JD5o3QgTVrSdcBp1fMJhrR9o", true),  // meteora_dlmm [5rCf1DM8] base_vault [19] w
    ("CoaxzEh8p5YyGLcj36Eo3cUThVJxeKCs7qvLAGDYwBcz", true),  // meteora_dlmm [5rCf1DM8] quote_vault [20] w
    ("59YuGWPunbchD2mbi9U7qvjWQKQReGeepn4ZSr9zz9Li", true),  // meteora_dlmm [5rCf1DM8] oracle [21] w
    ("DArpuuqJxNLRGQ8xq5ebZbobyjxSWWsPq8MqSZ2fUZLE", true),  // meteora_dlmm [5rCf1DM8] bitmap_ext [22] w
    ("EsMdTh8Ce3fdpQoLGhbuqrXKQnZTUQfVpKAbokm6QjYC", true),  // meteora_dlmm [5rCf1DM8] bin_array_buy_0 [23] w
    ("EsMdTh8Ce3fdpQoLGhbuqrXKQnZTUQfVpKAbokm6QjYC", true),  // meteora_dlmm [5rCf1DM8] bin_array_buy_1 [24] w
    ("BGm1tav58oGcsQJehL9WXBFXF7D27vZsKefj4xJKD5Y", true),  // meteora_dlmm [BGm1tav5] pool_id [25] w
    ("DwZz4S1Z1LBXomzmncQRVKCYhjCqSAMQ6RPKbUAadr7H", true),  // meteora_dlmm [BGm1tav5] base_vault [26] w
    ("4N22J4vW2juHocTntJNmXywSonYjkndCwahjZ2cYLDgb", true),  // meteora_dlmm [BGm1tav5] quote_vault [27] w
    ("ETc6tqgLrr7wXsH8u2QBK1CyXHX3kvV6WQjBz4cf3sCj", true),  // meteora_dlmm [BGm1tav5] oracle [28] w
    ("BzQsUBAbd21nrNDgc7D55EwnABC16uZJ41mgxxqYydHJ", true),  // meteora_dlmm [BGm1tav5] bitmap_ext [29] w
    ("D6ervPBg2dK8U77vdj5ptpQx1Ti9MDLjkyTgxKFCH6pm", true),  // meteora_dlmm [BGm1tav5] bin_array_buy_0 [30] w
    ("D6ervPBg2dK8U77vdj5ptpQx1Ti9MDLjkyTgxKFCH6pm", true),  // meteora_dlmm [BGm1tav5] bin_array_buy_1 [31] w
];

pub fn make_instruction_data(test_mode: bool) -> InstructionData {
    InstructionData {
        mints: 2,
        shared_statics_len: 3,
        pool_types: [3, 3, 3, 0, 0, 0, 0, 0],
        type_static_offsets: [0, 0, 0, 0, 0, 0, 0, 0],
        mode: 0,
        test: false,
        group_sizes: [0, 0, 0, 0],
        pool_fees: vec![],
    }
}
